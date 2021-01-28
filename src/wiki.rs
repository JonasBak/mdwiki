use std::ffi::OsStr;

use crate::config::{Config, User, WikiTree, MDWIKI_USER};
use crate::utils::*;
use crate::webapp::WebappState;

use async_std::fs;
use async_std::path::Path;

use rocket::tokio::sync::{mpsc, oneshot};

use mdbook::MDBook;

use git2::{IndexAddOption, Repository, Signature};

const SUMMARY_HEAD: &str = include_str!("../files/summary_head.md");

const THEME_OVERRIDE_SCRIPT: &str = include_str!("../files/theme_override_head.html.hbs");

const MDWIKI_README: &str = include_str!("../files/default_README.md");
const MDWIKI_BOOK_TOML: &str = include_str!("../files/default_book.toml");
const MDWIKI_GITIGNORE: &str = include_str!("../files/default_gitignore");

#[derive(Debug)]
pub enum WikiResponse {
    OK(Option<String>),
    BadRequest(Option<String>),
    NotAllowed(Option<String>),
    NotFound(Option<String>),
    Error(Option<String>),
}

impl WikiResponse {
    pub fn is_ok(&self) -> bool {
        match self {
            WikiResponse::OK(_) => true,
            _ => false,
        }
    }
    pub fn result(self) -> Result<Self, Self> {
        if self.is_ok() {
            Ok(self)
        } else {
            Err(self)
        }
    }
    pub fn msg(&self) -> Option<&String> {
        match self {
            WikiResponse::OK(msg)
            | WikiResponse::BadRequest(msg)
            | WikiResponse::NotAllowed(msg)
            | WikiResponse::NotFound(msg)
            | WikiResponse::Error(msg) => msg.as_ref(),
        }
    }
}

pub enum WikiRequest {
    CreateFile {
        user: User,
        file: Box<Path>,
        content: String,
        respond: oneshot::Sender<WikiResponse>,
    },
    EditFile {
        user: User,
        file: Box<Path>,
        content: String,
        respond: oneshot::Sender<WikiResponse>,
    },
}

pub struct WikiState {
    config: Config,
    rx: mpsc::Receiver<WikiRequest>,
}

impl WikiState {
    pub fn new() -> (WikiState, WebappState) {
        let (tx, rx) = mpsc::channel(100);

        (
            WikiState {
                config: Config::figment().extract().unwrap(),
                rx,
            },
            WebappState::new(tx),
        )
    }
    pub async fn setup(&self) -> Result<(), String> {
        info!(
            "setting up mdwiki with configuration: book path = {}",
            self.config.path
        );

        self.init_book().await?;
        let (book, _repo) = self.get_book()?;

        info!("running initial build",);
        book.build()
            .map_err(|e| format!("failed to build book: {}", e))?;

        Ok(())
    }
    pub async fn serve(mut self) {
        while let Some(req) = self.rx.recv().await {
            match req {
                WikiRequest::CreateFile {
                    user,
                    file,
                    content,
                    respond,
                } => {
                    if let Err(err) = self.create_file(&*file, content).await {
                        let _ = respond.send(err);
                        continue;
                    }
                    if let Err(err) = self
                        .on_created(&user, &*file)
                        .await
                        .map_err(log_warn)
                        .map_err(|_| WikiResponse::Error(None))
                    {
                        let _ = respond.send(err);
                        continue;
                    }
                    let _ = respond.send(WikiResponse::OK(None));
                }
                WikiRequest::EditFile {
                    user,
                    file,
                    content,
                    respond,
                } => {
                    if let Err(err) = self.edit_file(&*file, content).await {
                        let _ = respond.send(err);
                        continue;
                    }
                    if let Err(err) = self
                        .on_edited(&user, &*file)
                        .await
                        .map_err(log_warn)
                        .map_err(|_| WikiResponse::Error(None))
                    {
                        let _ = respond.send(err);
                        continue;
                    }

                    let _ = respond.send(WikiResponse::OK(None));
                }
            }
        }
    }
    async fn create_file(&self, file: &Path, content: String) -> Result<(), WikiResponse> {
        self.config.can_create(file).await.result()?;

        let path = Path::new(&self.config.path).join("src").join(&file);

        if let Some(parent) = path.parent() {
            if !parent.is_dir().await {
                fs::create_dir_all(parent)
                    .await
                    .map_err(log_warn)
                    .map_err(|_| WikiResponse::Error(None))?;
            }
        }

        let mut ancestors = file.ancestors();
        ancestors.next();
        for dir in ancestors {
            let index = Path::new(&self.config.path)
                .join("src")
                .join(&dir)
                .join("README.md");
            if !index.is_file().await {
                debug!("creating {}", index.to_string_lossy());
                fs::write(
                    index,
                    format!(
                        "# {}",
                        dir.file_stem()
                            .map(OsStr::to_str)
                            .flatten()
                            .unwrap_or("TODO")
                    ),
                )
                .await
                .map_err(log_warn)
                .map_err(|_| WikiResponse::Error(None))?;
            }
        }

        fs::write(path, content)
            .await
            .map_err(log_warn)
            .map_err(|_| WikiResponse::Error(None))?;

        Ok(())
    }
    async fn on_created(&self, user: &User, file: &Path) -> Result<(), String> {
        info!("running post-create hooks for {}", file.to_string_lossy());

        info!("updating summary");
        self.update_summary().await.map_err(log_warn)?;

        let (book, repo) = self.get_book().map_err(log_warn)?;

        info!("committing {}", file.to_string_lossy());
        self.commit(&repo, user, format!("Create {}", file.to_string_lossy()))
            .map_err(log_warn)?;

        info!("rebuilding book");
        book.build()
            .map_err(log_warn)
            .map_err(|e| format!("failed to build book: {}", e))?;

        Ok(())
    }
    async fn edit_file(&self, file: &Path, content: String) -> Result<(), WikiResponse> {
        self.config.can_edit(&file).await.result()?;

        let path = Path::new(&self.config.path).join("src").join(&file);
        fs::write(path, content)
            .await
            .map_err(log_warn)
            .map_err(|_| WikiResponse::Error(None))?;

        Ok(())
    }
    async fn on_edited(&self, user: &User, file: &Path) -> Result<(), String> {
        info!("running post-edit hooks for {}", file.to_string_lossy());
        let (book, repo) = self.get_book().map_err(log_warn)?;

        info!("committing changes to {}", file.to_string_lossy());
        self.commit(&repo, user, format!("Edit {}", file.to_string_lossy()))
            .map_err(log_warn)?;

        info!("rebuilding book");
        book.build()
            .map_err(log_warn)
            .map_err(|e| format!("failed to build book: {}", e))?;

        Ok(())
    }
    async fn init_book(&self) -> Result<(), String> {
        let book_path = Path::new(&self.config.path);
        let book_src_path = book_path.join("src");
        let repo = match Repository::open(&self.config.path) {
            Ok(repo) => {
                info!("using existing git repository");
                repo
            }
            Err(_) => {
                info!("could not find existing git repository, initializing new");

                Repository::init(&self.config.path)
                    .map_err(|e| format!("failed to init repo at '{}': {}", self.config.path, e))?
            }
        };
        if MDBook::load(&self.config.path).is_err() {
            info!(
                "could not find existing mdbook, creating new at {}",
                self.config.path
            );

            if !book_path.is_dir().await {
                fs::create_dir(&book_path).await.map_err(|e| {
                    format!("could not create directory '{}': {}", self.config.path, e)
                })?;
            }
            if !book_src_path.is_dir().await {
                fs::create_dir(&book_src_path).await.map_err(|e| {
                    format!(
                        "could not create directory '{}/src': {}",
                        self.config.path, e
                    )
                })?;
            }
            let book_images_path = book_src_path.join("images");
            if !book_images_path.is_dir().await {
                fs::create_dir(&book_images_path).await.map_err(|e| {
                    format!(
                        "could not create directory '{}/src/images': {}",
                        self.config.path, e
                    )
                })?;
            }

            fs::write(book_path.join("book.toml"), MDWIKI_BOOK_TOML)
                .await
                .map_err(|e| format!("could not write book.toml: {}", e))?;
            fs::write(book_path.join(".gitignore"), MDWIKI_GITIGNORE)
                .await
                .map_err(|e| format!("could not write gitignore: {}", e))?;
            fs::write(book_src_path.join("README.md"), MDWIKI_README)
                .await
                .map_err(|e| format!("could not write index file: {}", e))?;

            self.update_summary().await?;

            self.commit(&repo, &MDWIKI_USER, "Initial mdwiki commit".into())?;
        };
        let theme_dir = book_path.join("theme");
        let theme_path = theme_dir.join("head.hbs");
        if !theme_path.is_file().await {
            debug!("adding mdwiki theme script");
            if !theme_dir.is_dir().await {
                fs::create_dir(&theme_dir)
                    .await
                    .map_err(|_| "failed to create theme dir")?;
            }

            fs::write(&theme_path, THEME_OVERRIDE_SCRIPT)
                .await
                .map_err(|e| format!("failed to write theme script: {}", e))?;
        }
        Ok(())
    }
    fn get_book(&self) -> Result<(MDBook, Repository), String> {
        let repo = match Repository::open(&self.config.path) {
            Ok(repo) => {
                info!("using existing git repository");
                repo
            }
            Err(_) => {
                return Err(format!("could not find git repo at {}", self.config.path));
            }
        };
        let book = match MDBook::load(&self.config.path) {
            Ok(book) => {
                info!("using existing mdbook at {}", self.config.path);
                book
            }
            Err(_) => {
                return Err(format!("could not find book at {}", self.config.path));
            }
        };
        Ok((book, repo))
    }
    async fn update_summary(&self) -> Result<(), String> {
        let tree = self.config.get_wiki_tree().await;

        fn build_summary(summary: &mut String, tree: WikiTree) {
            use std::fmt::Write;
            match tree {
                WikiTree::File(path) => {
                    let level = path.ancestors().count() - 2;
                    let link_to = path.to_str().unwrap();
                    let page_title = path
                        .file_stem()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .replace("_", " ");
                    write!(
                        summary,
                        "{1:0$}- [{2}]({3})\n",
                        level * 2,
                        "",
                        page_title,
                        link_to
                    )
                    .unwrap();
                }
                WikiTree::Directory(path, children) => {
                    if &*path == Path::new("") {
                        summary.write_str(SUMMARY_HEAD).unwrap();
                    } else {
                        let level = path.ancestors().count() - 2;
                        let readme_path = path.join("README.md");
                        let link_to = readme_path.to_str().unwrap();
                        let page_title = path
                            .file_stem()
                            .map(|p| p.to_str())
                            .flatten()
                            .unwrap_or("README")
                            .replace("_", " ");
                        write!(
                            summary,
                            "{1:0$}- [{2}]({3})\n",
                            level * 2,
                            "",
                            page_title,
                            link_to
                        )
                        .unwrap();
                    }
                    for child in children {
                        build_summary(summary, child);
                    }
                }
            }
        }
        let mut summary = String::new();
        build_summary(&mut summary, tree);

        let summary_path = Path::new(&self.config.path).join("src/SUMMARY.md");
        fs::write(summary_path, summary)
            .await
            .map_err(|e| format!("could not write summary file: {}", e))?;

        Ok(())
    }
    fn commit(&self, repo: &Repository, user: &User, commit_message: String) -> Result<(), String> {
        let mut index = repo
            .index()
            .map_err(|e| format!("failed to get the index file: {}", e))?;
        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .map_err(|e| format!("failed to add files: {}", e))?;
        index
            .write()
            .map_err(|e| format!("failed to write to index: {}", e))?;
        let tree_id = index
            .write_tree()
            .map_err(|e| format!("failed to write tree: {}", e))?;

        {
            let sig = Signature::now(&user.username, "mdwiki@example.com")
                .map_err(|e| format!("failed to get signature: {}", e))?;
            let tree = repo
                .find_tree(tree_id)
                .map_err(|e| format!("failed to find tree: {}", e))?;
            let parent = repo
                .head()
                .ok()
                .map(|head| head.peel_to_commit().ok())
                .flatten();
            repo.commit(
                Some("HEAD"),
                &sig,
                &sig,
                &commit_message,
                &tree,
                &parent.iter().collect::<Vec<_>>(),
            )
            .map_err(|e| format!("failed to create initial commit: {}", e))?;
        }
        Ok(())
    }
}
