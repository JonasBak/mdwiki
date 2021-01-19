#![feature(proc_macro_hygiene, decl_macro)]

#[macro_use]
extern crate rocket;

#[macro_use]
extern crate log;

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::fs;
use std::fs::OpenOptions;
use std::io::prelude::*;
use std::path::{Path, PathBuf};

use rocket::http::Status;
use rocket::request::Form;
use rocket::response::Redirect;
use rocket::State;
use rocket_contrib::serve::StaticFiles;
use rocket_contrib::templates::Template;

use mdbook::config::Config;
use mdbook::MDBook;

use git2::{IndexAddOption, Repository};

use serde::Serialize;

fn log_warn<T: std::fmt::Display>(err: T) -> T {
    warn!("{}", err);
    err
}

const RESERVED_NAMES: &[&str] = &["SUMMARY.md", "index.md"];

fn is_reserved_name(path: &Path) -> bool {
    RESERVED_NAMES
        .iter()
        .find(|reserved| path.ends_with(reserved))
        .is_some()
}

const MDWIKI_README: &str = r#"
# mdwiki

> Lorem ipsum dolor sit amet, consectetur adipiscing elit. In efficitur augue sed scelerisque finibus.

## Instructions

Lorem ipsum dolor sit amet, consectetur adipiscing elit. Mauris consectetur quis magna ut convallis. Nam tincidunt efficitur consectetur. Fusce erat massa, convallis a erat sed, convallis congue arcu. Sed auctor turpis quis diam euismod, in venenatis ipsum luctus. Praesent eget lobortis elit, at luctus sem.
"#;

const THEME_OVERRIDE_SCRIPT: &str = r#"
<script type="text/javascript">
    window.addEventListener("load", function() {
        const buttonDiv = document.getElementsByClassName("right-buttons")[0];

        editLink = document.createElement("a");
        editLink.href = "/edit/{{ path }}".replace(/index.md$/, "README.md");
        editLink.title = "Edit this page";

        editIcon = document.createElement("i");
        editIcon.className = "fa fa-edit";

        editLink.appendChild(editIcon);
        buttonDiv.appendChild(editLink);

        newLink = document.createElement("a");
        newLink.href = "/new";
        newLink.title = "Create new page";

        newIcon = document.createElement("i");
        newIcon.className = "fa fa-plus";

        newLink.appendChild(newIcon);
        buttonDiv.appendChild(newLink);
    });
</script>
"#;

#[derive(Serialize)]
struct NewContext {}

#[derive(FromForm)]
struct NewForm {
    file: String,
    content: String,
}

#[get("/")]
async fn new_page() -> Template {
    let context = NewContext {};
    Template::render("new_page", &context)
}

#[post("/", data = "<form>")]
async fn new_page_post(
    form: Form<NewForm>,
    state: State<'_, AppState>,
) -> Result<Redirect, Status> {
    let file = Path::new(&form.file);
    // TODO handle path traversal
    let path = Path::new(&state.book_path).join("src").join(&file);
    if !state.can_create(&path) {
        return Err(Status::BadRequest);
    }

    if let Some(parent) = path.parent() {
        if !parent.is_dir() {
            fs::create_dir_all(parent)
                .map_err(log_warn)
                .map_err(|_| Status::InternalServerError)?;
        }
    }

    let mut ancestors = file.ancestors();
    ancestors.next();
    for dir in ancestors {
        let index = Path::new(&state.book_path)
            .join("src")
            .join(&dir)
            .join("README.md");
        if !index.is_file() {
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
            .map_err(log_warn)
            .map_err(|_| Status::InternalServerError)?;
        }
    }

    fs::write(path, &form.content)
        .map_err(log_warn)
        .map_err(|_| Status::InternalServerError)?;

    state
        .on_created(&file)
        .map_err(log_warn)
        .map_err(|_| Status::InternalServerError)?;

    let html_file = Path::new(&form.file).with_extension("html");
    return Ok(Redirect::to(format!(
        "/{}",
        html_file
            .to_str()
            .ok_or_else(|| Status::InternalServerError)?
            .to_string()
    )));
}

#[derive(Serialize)]
struct EditContext {
    file: PathBuf,
    content: String,
}

#[derive(FromForm)]
struct EditForm {
    content: String,
}

#[get("/<file..>")]
async fn edit_page(file: PathBuf, state: State<'_, AppState>) -> Result<Template, Status> {
    let path = Path::new(&state.book_path).join("src").join(&file);
    if !state.can_edit(&path) {
        return Err(Status::NotFound);
    }
    let content = fs::read_to_string(&path)
        .map_err(log_warn)
        .map_err(|_| Status::NotFound)?;
    let context = EditContext { file, content };
    Ok(Template::render("edit_page", &context))
}

#[post("/<file..>", data = "<form>")]
async fn edit_page_post(
    file: PathBuf,
    form: Form<EditForm>,
    state: State<'_, AppState>,
) -> Result<Redirect, Status> {
    let path = Path::new(&state.book_path).join("src").join(&file);
    if !state.can_edit(&path) {
        return Err(Status::NotFound);
    }
    fs::write(path, &form.content)
        .map_err(log_warn)
        .map_err(|_| Status::InternalServerError)?;

    state
        .on_edited(&file)
        .map_err(log_warn)
        .map_err(|_| Status::InternalServerError)?;

    let html_file = file.clone().with_extension("html");
    return Ok(Redirect::to(format!(
        "/{}",
        html_file
            .to_str()
            .ok_or_else(|| Status::InternalServerError)?
            .to_string()
    )));
}

struct AppState {
    book_path: String,
}

impl AppState {
    fn setup(&self) -> Result<Box<Path>, String> {
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
    fn on_created(&self, file: &Path) -> Result<(), String> {
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
    fn on_edited(&self, file: &PathBuf) -> Result<(), String> {
        info!("running post-edit hooks for {}", file.to_string_lossy());
        let (book, repo) = self.get_book(false)?;

        info!("committing changes to {}", file.to_string_lossy());
        self.commit(&repo, format!("Edit {}", file.to_string_lossy()))?;

        info!("rebuilding book");
        book.build()
            .map_err(|e| format!("failed to build book: {}", e))?;

        Ok(())
    }
    fn get_book(&self, init: bool) -> Result<(MDBook, Repository), String> {
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

                if !Path::new(&self.book_path).is_dir() {
                    fs::create_dir(&self.book_path).map_err(|e| {
                        format!("could not create directory '{}': {}", self.book_path, e)
                    })?;
                }

                let mut cfg = Config::default();
                cfg.book.title = Some("mdwiki".into());
                cfg.book.authors.push("mdwiki".into());

                MDBook::init(&self.book_path)
                    .create_gitignore(true)
                    .with_config(cfg)
                    .build()
                    .map_err(|_| format!("failed to initialize wiki at '{}'", self.book_path))?;

                fs::write(
                    Path::new(&self.book_path).join("src/README.md"),
                    MDWIKI_README,
                )
                .map_err(|e| format!("could not write index file: {}", e))?;

                self.update_summary()?;

                if let Ok(mut gitignore) = OpenOptions::new()
                    .write(true)
                    .append(true)
                    .open(Path::new(&self.book_path).join(".gitignore"))
                {
                    let _ = writeln!(gitignore, "theme/head.hbs");
                }

                MDBook::load(&self.book_path).unwrap()
            }
        };
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

                let repo = Repository::init(&self.book_path)
                    .map_err(|e| format!("failed to init repo at '{}': {}", self.book_path, e))?;

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
                    repo.commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])
                        .map_err(|e| format!("failed to create initial commit: {}", e))?;
                }

                repo
            }
        };
        let theme_dir = Path::new(&self.book_path).join("theme");
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
    fn update_summary(&self) -> Result<(), String> {
        let mut queue = vec![Some(Path::new(&self.book_path).join("src"))];
        let mut files = Vec::new();
        let mut i = 0;
        while i < queue.len() {
            let path = queue[i].take().unwrap();
            if path.is_dir() {
                for entry in fs::read_dir(path).unwrap() {
                    queue.push(Some(entry.unwrap().path()));
                }
            } else {
                files.push(path);
            }
            i += 1;
        }
        let prefix = Path::new(&self.book_path).join("src");
        let mut relative_md_files = files
            .iter()
            .filter(|path| path.extension().map(|ext| ext == "md").unwrap_or(false))
            .filter(|path| !is_reserved_name(path))
            .filter_map(|path| path.strip_prefix(&prefix).ok())
            .collect::<Vec<_>>();
        relative_md_files.sort_by(|a, b| {
            if a.parent() != b.parent() {
                return a.cmp(b);
            }
            if Some(OsStr::new("README")) == a.file_stem() {
                return Ordering::Less;
            }
            if Some(OsStr::new("README")) == b.file_stem() {
                return Ordering::Greater;
            }
            a.cmp(b)
        });
        let summary = relative_md_files
            .into_iter()
            .map(|mut path| {
                let link_to = path.to_str().unwrap_or("");
                if Some(OsStr::new("README")) == path.file_stem() {
                    if let Some(parent) = path.parent() {
                        if parent.parent().is_some() {
                            path = parent;
                        }
                    }
                }
                let level = path.ancestors().count() - 2;
                let page_title = path
                    .file_stem()
                    .map(|f| f.to_str())
                    .flatten()
                    .unwrap_or("")
                    .replace("_", " ");
                return format!("{1:0$}- [{2}](./{3})", level * 2, "", page_title, link_to);
            })
            .collect::<Vec<_>>()
            .join("\n");

        let summary_path = Path::new(&self.book_path).join("src/SUMMARY.md");
        fs::write(summary_path, summary)
            .map_err(|e| format!("could not write summary file: {}", e))?;

        Ok(())
    }
    fn commit(&self, repo: &Repository, commit_message: String) -> Result<(), String> {
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
                .map_err(|e| format!("failed to get parent: {}", e))?
                .peel_to_commit()
                .map_err(|e| format!("failed to get parent: {}", e))?;
            repo.commit(Some("HEAD"), &sig, &sig, &commit_message, &tree, &[&parent])
                .map_err(|e| format!("failed to create initial commit: {}", e))?;
        }
        Ok(())
    }
    fn can_edit(&self, path: &Path) -> bool {
        let prefix = Path::new(&self.book_path).join("src");
        if !path.starts_with(&prefix) {
            return false;
        } else if path.extension().map(|ext| ext != "md").unwrap_or(true) {
            return false;
        } else if !path.is_file() {
            return false;
        } else if is_reserved_name(path) {
            return false;
        }
        true
    }
    fn can_create(&self, path: &Path) -> bool {
        let prefix = Path::new(&self.book_path).join("src");
        if !path.starts_with(&prefix) {
            return false;
        } else if path
            .strip_prefix(&prefix)
            .map(|path| path.ancestors().count() - 2)
            .unwrap_or(99)
            > 3
        {
            return false;
        } else if path.extension().map(|ext| ext != "md").unwrap_or(true) {
            return false;
        } else if path.is_file() {
            return false;
        } else if is_reserved_name(path) {
            return false;
        }
        true
    }
}

#[rocket::main]
async fn main() {
    env_logger::init();

    let book_path = "/tmp/mdwiki".into();

    let state = AppState { book_path };

    let build_path = state.setup().unwrap();

    rocket::ignite()
        .attach(Template::fairing())
        .manage(state)
        .mount("/new", routes![new_page, new_page_post])
        .mount("/edit", routes![edit_page, edit_page_post])
        .mount("/", StaticFiles::from(build_path))
        .launch()
        .await
        .unwrap();
}
