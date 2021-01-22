use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::utils::*;

use mdbook::MDBook;

use git2::{IndexAddOption, Repository};

const SUMMARY_HEAD: &str = include_str!("../files/summary_head.md");

const THEME_OVERRIDE_SCRIPT: &str = include_str!("../files/theme_override_head.html.hbs");

const MDWIKI_README: &str = include_str!("../files/default_README.md");
const MDWIKI_BOOK_TOML: &str = include_str!("../files/default_book.toml");
const MDWIKI_GITIGNORE: &str = include_str!("../files/default_gitignore");

enum WikiTree {
    File(Box<Path>),
    Directory(Box<Path>, Vec<WikiTree>),
}

impl WikiTree {
    fn path(&self) -> &Path {
        match self {
            WikiTree::File(path) => &path,
            WikiTree::Directory(path, _) => &path,
        }
    }
}

pub struct AppState {
    pub book_path: String,
    pub dir_lock: Arc<Mutex<()>>,
}

impl AppState {
    pub fn setup(&self) -> Result<Box<Path>, String> {
        info!(
            "setting up mdwiki with configuration: book path = {}",
            self.book_path
        );

        let (book, _repo) = self.get_book(true)?;

        info!("running initial build",);
        book.build()
            .map_err(|e| format!("failed to build book: {}", e))?;

        let build_path = Path::new(&self.book_path).join(book.config.build.build_dir);
        Ok(build_path.into_boxed_path())
    }
    pub fn on_created(&self, file: &Path) -> Result<(), String> {
        info!("running post-create hooks for {}", file.to_string_lossy());

        info!("updating summary");
        self.update_summary()?;

        let (book, repo) = self.get_book(false)?;

        info!("committing {}", file.to_string_lossy());
        self.commit(&repo, format!("Create {}", file.to_string_lossy()))?;

        info!("rebuilding book");
        book.build()
            .map_err(|e| format!("failed to build book: {}", e))?;

        Ok(())
    }
    pub fn on_edited(&self, file: &PathBuf) -> Result<(), String> {
        info!("running post-edit hooks for {}", file.to_string_lossy());
        let (book, repo) = self.get_book(false)?;

        info!("committing changes to {}", file.to_string_lossy());
        self.commit(&repo, format!("Edit {}", file.to_string_lossy()))?;

        info!("rebuilding book");
        book.build()
            .map_err(|e| format!("failed to build book: {}", e))?;

        Ok(())
    }
    pub fn get_book(&self, init: bool) -> Result<(MDBook, Repository), String> {
        let book_path = Path::new(&self.book_path);
        let book_src_path = book_path.join("src");
        let repo = match Repository::open(&self.book_path) {
            Ok(repo) => {
                info!("using existing git repository");
                repo
            }
            Err(_) => {
                if !init {
                    return Err(format!("could not find git repo at {}", self.book_path));
                }
                info!("could not find existing git repository, initializing new");

                Repository::init(&self.book_path)
                    .map_err(|e| format!("failed to init repo at '{}': {}", self.book_path, e))?
            }
        };
        let book = match MDBook::load(&self.book_path) {
            Ok(book) => {
                info!("using existing mdbook at {}", self.book_path);
                book
            }
            Err(_) => {
                if !init {
                    return Err(format!("could not find book at {}", self.book_path));
                }
                info!(
                    "could not find existing mdbook, creating new at {}",
                    self.book_path
                );

                if !book_path.is_dir() {
                    fs::create_dir(&book_path).map_err(|e| {
                        format!("could not create directory '{}': {}", self.book_path, e)
                    })?;
                }
                if !book_src_path.is_dir() {
                    fs::create_dir(&book_src_path).map_err(|e| {
                        format!("could not create directory '{}/src': {}", self.book_path, e)
                    })?;
                }
                let book_images_path = book_src_path.join("images");
                if !book_images_path.is_dir() {
                    fs::create_dir(&book_images_path).map_err(|e| {
                        format!(
                            "could not create directory '{}/src/images': {}",
                            self.book_path, e
                        )
                    })?;
                }

                fs::write(book_path.join("book.toml"), MDWIKI_BOOK_TOML)
                    .map_err(|e| format!("could not write book.toml: {}", e))?;
                fs::write(book_path.join(".gitignore"), MDWIKI_GITIGNORE)
                    .map_err(|e| format!("could not write gitignore: {}", e))?;
                fs::write(book_src_path.join("README.md"), MDWIKI_README)
                    .map_err(|e| format!("could not write index file: {}", e))?;

                self.update_summary()?;

                let book = MDBook::load(&self.book_path).unwrap();

                self.commit(&repo, "Initial mdwiki commit".into())?;

                book
            }
        };
        let theme_dir = book_path.join("theme");
        let theme_path = theme_dir.join("head.hbs");
        if !theme_path.is_file() && init {
            debug!("adding mdwiki theme script");
            if !theme_dir.is_dir() {
                fs::create_dir(&theme_dir).map_err(|_| "failed to create theme dir")?;
            }

            fs::write(&theme_path, THEME_OVERRIDE_SCRIPT)
                .map_err(|e| format!("failed to write theme script: {}", e))?;
        }
        Ok((book, repo))
    }
    fn get_wiki_tree(&self) -> WikiTree {
        fn visit(prefix: &Path, path: &Path) -> Option<WikiTree> {
            let relative_path = path.strip_prefix(&prefix).unwrap();
            if path.is_dir() {
                if relative_path.starts_with("images") {
                    return None;
                }
                let mut children = fs::read_dir(path)
                    .unwrap()
                    .into_iter()
                    .map(|entry| visit(prefix, &entry.unwrap().path()))
                    .filter_map(|a| a)
                    .collect::<Vec<_>>();
                children.sort_by(|a, b| a.path().cmp(b.path()));
                return Some(WikiTree::Directory(
                    relative_path.to_path_buf().into_boxed_path(),
                    children,
                ));
            } else {
                if path.extension().map(|ext| ext != "md").unwrap_or(true) {
                    return None;
                } else if path.file_stem().map(|ext| ext == "README").unwrap_or(true) {
                    return None;
                } else if is_reserved_name(relative_path) {
                    return None;
                }
                return Some(WikiTree::File(
                    relative_path.to_path_buf().into_boxed_path(),
                ));
            }
        }
        let prefix = Path::new(&self.book_path).join("src");
        visit(&prefix, &Path::new(&self.book_path).join("src")).unwrap()
    }
    pub fn update_summary(&self) -> Result<(), String> {
        let tree = self.get_wiki_tree();

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

        let summary_path = Path::new(&self.book_path).join("src/SUMMARY.md");
        fs::write(summary_path, summary)
            .map_err(|e| format!("could not write summary file: {}", e))?;

        Ok(())
    }
    pub fn commit(&self, repo: &Repository, commit_message: String) -> Result<(), String> {
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
            let sig = repo
                .signature()
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
    pub fn can_edit(&self, path: &Path) -> bool {
        if !path_is_simple(path) {
            return false;
        } else if path.extension().map(|ext| ext != "md").unwrap_or(true) {
            return false;
        } else if is_reserved_name(path) {
            return false;
        }

        let full_path = Path::new(&self.book_path).join("src").join(&path);

        if !full_path.is_file() {
            return false;
        }
        true
    }
    pub fn can_create(&self, path: &Path) -> bool {
        if !path_is_simple(path) {
            return false;
        } else if path.extension().map(|ext| ext != "md").unwrap_or(true) {
            return false;
        } else if is_reserved_name(path) {
            return false;
        } else if path.ancestors().count() > 5 {
            return false;
        }

        let full_path = Path::new(&self.book_path).join("src").join(&path);

        if full_path.is_file() {
            return false;
        }
        true
    }
}
