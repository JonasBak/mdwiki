#![feature(proc_macro_hygiene, decl_macro)]

#[macro_use]
extern crate rocket;

#[macro_use]
extern crate log;

use std::fs;
use std::fs::File;
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

const THEME_OVERRIDE_SCRIPT: &[u8] = br#"
<script type="text/javascript">
    window.addEventListener("load", function() {
        const buttonDiv = document.getElementsByClassName("right-buttons")[0];

        editLink = document.createElement("a");
        editLink.href = "/edit/{{ path }}";
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
    let path = Path::new(&state.book_path).join("src").join(&form.file);
    if !state.can_create(&path) {
        return Err(Status::BadRequest);
    }
    fs::write(path, &form.content)
        .map_err(log_warn)
        .map_err(|_| Status::InternalServerError)?;

    state
        .on_created(&form.file)
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
    fn on_created(&self, file: &String) -> Result<(), String> {
        info!("running post-create hooks for {}", file);
        let (book, repo) = self.get_book(false)?;

        info!("committing {}", file);
        self.commit(&repo, format!("Create {}", file))?;

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
                    .map_err(|_| format!("failed to initialize wiki at '{}'", self.book_path))?
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
        let theme_path = theme_dir.join("header.hbs");
        // TODO ignore theme dir
        if !theme_path.is_file() && init {
            debug!("adding mdwiki theme script");
            if !theme_dir.is_dir() {
                fs::create_dir(&theme_dir).map_err(|_| "failed to create theme dir")?;
            }
            let mut file = File::create(&theme_path).map_err(|_| "failed to create theme file")?;
            file.write_all(THEME_OVERRIDE_SCRIPT)
                .map_err(|_| "failed to write theme override script")?;
        }
        Ok((book, repo))
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
        if path.extension().map(|ext| ext != "md").unwrap_or(true) {
            return false;
        } else if !path.is_file() {
            return false;
        }
        true
    }
    fn can_create(&self, path: &Path) -> bool {
        if path.extension().map(|ext| ext != "md").unwrap_or(true) {
            return false;
        } else if path.is_file() {
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
