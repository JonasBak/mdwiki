#![feature(proc_macro_hygiene, decl_macro)]

#[macro_use]
extern crate rocket;

#[macro_use]
extern crate log;

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use rocket::data::{Data, ToByteUnit};
use rocket::http::{ContentType, Status};
use rocket::request::Form;
use rocket::response::Redirect;
use rocket::State;
use rocket_contrib::helmet::SpaceHelmet;
use rocket_contrib::serve::StaticFiles;
use rocket_contrib::templates::Template;

use mdbook::MDBook;

use git2::{IndexAddOption, Repository};

use serde::Serialize;

use rand::Rng;

fn log_warn<T: std::fmt::Display>(err: T) -> T {
    warn!("{}", err);
    err
}

const RESERVED_NAMES: &[&str] = &["SUMMARY.md", "index.md"];
const RESERVED_PREFIXES: &[&str] = &["new", "edit", "upload", "images"];

fn is_reserved_name(path: &Path) -> bool {
    RESERVED_NAMES
        .iter()
        .find(|reserved| path.ends_with(reserved))
        .is_some()
        || RESERVED_PREFIXES
            .iter()
            .find(|reserved| path.starts_with(reserved))
            .is_some()
}

fn path_is_simple(path: &Path) -> bool {
    path.components()
        .find(|comp| match comp {
            Component::Normal(_) => false,
            _ => true,
        })
        .is_none()
}

fn rand_safe_string(length: usize) -> String {
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz";

    let mut rng = rand::thread_rng();

    (0..length)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

const SUMMARY_HEAD: &str = include_str!("../files/summary_head.md");

const THEME_OVERRIDE_SCRIPT: &str = include_str!("../files/theme_override_head.html.hbs");

const MDWIKI_README: &str = include_str!("../files/default_README.md");
const MDWIKI_BOOK_TOML: &str = include_str!("../files/default_book.toml");
const MDWIKI_GITIGNORE: &str = include_str!("../files/default_gitignore");

#[derive(Serialize)]
struct NewContext {}

#[derive(FromForm)]
struct NewForm {
    file: String,
    content: String,
}

#[get("/")]
fn new_page() -> Template {
    let context = NewContext {};
    Template::render("new_page", &context)
}

#[post("/", data = "<form>")]
fn new_page_post(form: Form<NewForm>, state: State<'_, AppState>) -> Result<Redirect, Status> {
    // TODO check for legal characters in path
    let form_file = form.file.replace(" ", "_");
    let file = Path::new(&form_file);
    if !state.can_create(&file) {
        return Err(Status::BadRequest);
    }

    {
        let _ = state.dir_lock.lock().unwrap();

        let path = Path::new(&state.book_path).join("src").join(&file);

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
    }

    let html_file = Path::new(&form.file).with_extension("html");
    return Ok(Redirect::to(format!(
        "/{}",
        html_file
            .to_str()
            .ok_or_else(|| Status::InternalServerError)?
            .replace("README.html", "")
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
fn edit_page(file: PathBuf, state: State<'_, AppState>) -> Result<Template, Status> {
    if !state.can_edit(&file) {
        return Err(Status::NotFound);
    }
    let path = Path::new(&state.book_path).join("src").join(&file);
    let content = fs::read_to_string(&path)
        .map_err(log_warn)
        .map_err(|_| Status::NotFound)?;
    let context = EditContext { file, content };
    Ok(Template::render("edit_page", &context))
}

#[post("/<file..>", data = "<form>")]
fn edit_page_post(
    file: PathBuf,
    form: Form<EditForm>,
    state: State<'_, AppState>,
) -> Result<Redirect, Status> {
    if !state.can_edit(&file) {
        return Err(Status::NotFound);
    }

    {
        let _ = state.dir_lock.lock().unwrap();

        let path = Path::new(&state.book_path).join("src").join(&file);
        fs::write(path, &form.content)
            .map_err(log_warn)
            .map_err(|_| Status::InternalServerError)?;

        state
            .on_edited(&file)
            .map_err(log_warn)
            .map_err(|_| Status::InternalServerError)?;
    }

    let html_file = file.with_extension("html");
    return Ok(Redirect::to(format!(
        "/{}",
        html_file
            .to_str()
            .ok_or_else(|| Status::InternalServerError)?
            .replace("README.html", "")
            .to_string()
    )));
}

#[post("/image", data = "<data>")]
async fn upload_image(
    data: Data,
    content_type: &ContentType,
    state: State<'_, AppState>,
) -> Result<String, ()> {
    let filename = rand_safe_string(16);
    let extension = if *content_type == ContentType::JPEG {
        "jpg"
    } else if *content_type == ContentType::GIF {
        "gif"
    } else if *content_type == ContentType::PNG {
        "png"
    } else if *content_type == ContentType::BMP {
        "bmp"
    } else {
        return Err(());
    };

    let file_path = Path::new(&state.book_path)
        .join("src/images")
        .join(&filename)
        .with_extension(&extension);

    data.open(8_u8.mebibytes())
        .stream_to_file(file_path)
        .await
        .map_err(log_warn)
        .map_err(|_| ())?;

    Ok(format!("/images/{}.{}", filename, extension))
}

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

struct AppState {
    book_path: String,
    dir_lock: Arc<Mutex<()>>,
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
                if path.starts_with("images") {
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
    fn update_summary(&self) -> Result<(), String> {
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
    fn can_edit(&self, path: &Path) -> bool {
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
    fn can_create(&self, path: &Path) -> bool {
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

fn rocket(state: AppState, static_path: &Path) -> rocket::Rocket {
    rocket::ignite()
        .attach(Template::fairing())
        .attach(SpaceHelmet::default())
        .manage(state)
        .mount("/new", routes![new_page, new_page_post])
        .mount("/edit", routes![edit_page, edit_page_post])
        .mount("/upload", routes![upload_image,])
        .mount("/", StaticFiles::from(static_path))
}

#[rocket::main]
async fn main() {
    env_logger::init();

    let book_path = env::var("MDWIKI_PATH").expect("set mdwiki path with the MDWIKI_PATH variable");

    let state = AppState {
        book_path,
        dir_lock: Arc::new(Mutex::new(())),
    };

    let build_path = state.setup().unwrap();

    rocket(state, &*build_path).launch().await.unwrap();
}

#[cfg(test)]
mod test {
    use std::path::Path;

    use rocket::http::{ContentType, Status};
    use rocket::local::blocking::Client;

    fn get_rocket_instance(test_id: &'static str) -> (rocket::Rocket, Box<Path>) {
        use super::*;

        let book_path = Path::new("./target/tests").join(format!("mdwiki_{}", test_id));

        if book_path.is_dir() {
            std::fs::remove_dir_all(&book_path).unwrap();
        }

        let state = AppState {
            book_path: book_path.to_str().unwrap().to_string(),
            dir_lock: Arc::new(Mutex::new(())),
        };

        let build_path = state.setup().unwrap();

        (rocket(state, &*build_path), book_path.into_boxed_path())
    }

    #[test]
    fn bootstrap_wiki() {
        let (rocket, _book_path) = get_rocket_instance("bootstrap");

        let client = Client::tracked(rocket).expect("valid rocket instance");

        assert_eq!(client.get("/index.html").dispatch().status(), Status::Ok);
        assert_eq!(client.get("/SUMMARY.html").dispatch().status(), Status::Ok);
        assert_eq!(client.get("/new").dispatch().status(), Status::Ok);
        assert_eq!(
            client.get("/edit/README.md").dispatch().status(),
            Status::Ok
        );
        assert_eq!(
            client.get("/edit/chapter_1.md").dispatch().status(),
            Status::NotFound
        );

        let response = client.get("/").dispatch();
        assert_eq!(response.status(), Status::Ok);
        assert!(response
            .into_string()
            .unwrap()
            .contains(r#"// mdwiki theme override script to add "edit" and "new" buttons"#));
    }

    #[test]
    fn new_page() {
        let (rocket, _book_path) = get_rocket_instance("new_page");

        let client = Client::tracked(rocket).expect("valid rocket instance");

        let response = client
            .post("/new")
            .header(ContentType::Form)
            .body("file=newfile.md&content=NEWPAGE")
            .dispatch();

        assert_eq!(response.status(), Status::SeeOther);
        assert_eq!(
            response.headers().get_one("location"),
            Some("/newfile.html")
        );

        let response = client.get("/newfile.html").dispatch();
        assert_eq!(response.status(), Status::Ok);
        assert!(response.into_string().unwrap().contains("NEWPAGE"));
    }

    #[test]
    fn new_page_with_dirs() {
        let (rocket, _book_path) = get_rocket_instance("new_page_with_dirs");

        let client = Client::tracked(rocket).expect("valid rocket instance");

        let response = client
            .post("/new")
            .header(ContentType::Form)
            .body("file=newdir/newfile.md&content=NEWPAGE")
            .dispatch();

        assert_eq!(response.status(), Status::SeeOther);
        assert_eq!(
            response.headers().get_one("location"),
            Some("/newdir/newfile.html")
        );

        assert_eq!(client.get("/newdir/").dispatch().status(), Status::Ok);
        assert_eq!(
            client.get("/newdir/index.html").dispatch().status(),
            Status::Ok
        );
        assert_eq!(
            client.get("/newdir/newfile.html").dispatch().status(),
            Status::Ok
        );
    }

    #[test]
    fn edit_page() {
        let (rocket, _book_path) = get_rocket_instance("edit_page");

        let client = Client::tracked(rocket).expect("valid rocket instance");

        let response = client
            .post("/edit/README.md")
            .header(ContentType::Form)
            .body("content=EDITEDCONTENT")
            .dispatch();

        assert_eq!(response.status(), Status::SeeOther);
        assert_eq!(response.headers().get_one("location"), Some("/"));

        let response = client.get("/index.html").dispatch();
        assert_eq!(response.status(), Status::Ok);
        assert!(response.into_string().unwrap().contains("EDITEDCONTENT"));
    }
}
