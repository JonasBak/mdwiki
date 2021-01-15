#![feature(proc_macro_hygiene, decl_macro)]

#[macro_use]
extern crate rocket;

use std::fs;
use std::fs::File;
use std::io::prelude::*;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rocket::request::{self, FromRequest, Request};
use rocket_contrib::serve::StaticFiles;
use rocket_contrib::templates::Template;

use mdbook::config::Config;
use mdbook::MDBook;

use git2::{Index, IndexAddOption, Repository};

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

#[get("/")]
async fn edit_home() -> String {
    "Hello World!".into()
}

struct AppState {
    book_path: String,
}

impl AppState {
    fn setup(&self) -> Result<Box<Path>, String> {
        let (book, repo) = self.get_book_or_initialize()?;
        book.build().map_err(|_| "failed to build book")?;
        let build_path = Path::new(&self.book_path).join(book.config.build.build_dir);
        Ok(build_path.into_boxed_path())
    }
    fn get_book_or_initialize(&self) -> Result<(MDBook, Repository), String> {
        let book = match MDBook::load(&self.book_path) {
            Ok(book) => book,
            Err(_) => {
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
            Ok(repo) => repo,
            Err(_) => {
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
        if !theme_path.is_file() {
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
    let book_path = "/tmp/mdwiki".into();

    let state = AppState { book_path };

    let build_path = state.setup().unwrap();

    rocket::ignite()
        .attach(Template::fairing())
        .manage(state)
        .mount("/edit", routes![edit_home,])
        .mount("/", StaticFiles::from(build_path))
        .launch()
        .await
        .unwrap();
}
