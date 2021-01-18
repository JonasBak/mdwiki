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

#[get("/")]
async fn new_page() -> Template {
    let context = NewContext {};
    Template::render("new_page", &context)
}

#[derive(Serialize)]
struct EditContext {
    file: PathBuf,
    file_content: String,
}

#[derive(FromForm)]
struct EditForm {
    content: String,
}

#[get("/<file..>")]
async fn edit_page(file: PathBuf, state: State<'_, AppState>) -> Result<Template, Status> {
    let path = Path::new(&state.book_path).join("src").join(&file);
    if path.extension().map(|ext| ext != "md").unwrap_or(true) {
        return Err(Status::NotFound);
    } else if !path.is_file() {
        return Err(Status::NotFound);
    }
    let file_content = fs::read_to_string(&path)
        .map_err(log_warn)
        .map_err(|_| Status::NotFound)?;
    let context = EditContext { file, file_content };
    Ok(Template::render("edit_page", &context))
}

#[post("/<file..>", data = "<form>")]
async fn edit_page_post(
    file: PathBuf,
    form: Form<EditForm>,
    state: State<'_, AppState>,
) -> Result<Redirect, Status> {
    let path = Path::new(&state.book_path).join("src").join(&file);
    if path.extension().map(|ext| ext != "md").unwrap_or(true) {
        return Err(Status::NotFound);
    } else if !path.is_file() {
        return Err(Status::NotFound);
    }
    fs::write(path, &form.content)
        .map_err(log_warn)
        .map_err(|_| Status::InternalServerError)?;

    state
        .on_edited()
        .map_err(log_warn)
        .map_err(|_| Status::InternalServerError)?;

    let mut html_file = file.clone();
    html_file.set_extension("html");
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

        let (book, _repo) = self.get_book_or_initialize()?;

        book.build()
            .map_err(|e| format!("failed to build book: {}", e))?;

        let build_path = Path::new(&self.book_path).join(book.config.build.build_dir);
        Ok(build_path.into_boxed_path())
    }
    fn on_edited(&self) -> Result<(), String> {
        // TODO add & commit
        // TODO rebuild mdbook
        Ok(())
    }
    fn get_book_or_initialize(&self) -> Result<(MDBook, Repository), String> {
        let book = match MDBook::load(&self.book_path) {
            Ok(book) => {
                info!("using existing mdbook at {}", self.book_path);
                book
            }
            Err(_) => {
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
        if !theme_path.is_file() {
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
        .mount("/new", routes![new_page,])
        .mount("/edit", routes![edit_page, edit_page_post])
        .mount("/", StaticFiles::from(build_path))
        .launch()
        .await
        .unwrap();
}
