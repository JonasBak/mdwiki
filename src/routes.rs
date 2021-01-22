use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::utils::*;
use crate::wiki::AppState;

use rocket::data::{Data, ToByteUnit};
use rocket::http::{ContentType, Status};
use rocket::request::Form;
use rocket::response::Redirect;
use rocket::State;

use rocket_contrib::templates::Template;

use serde::Serialize;

#[derive(Serialize)]
struct NewContext {}

#[derive(FromForm)]
pub struct NewForm {
    file: String,
    content: String,
}

#[get("/")]
pub fn new_page() -> Template {
    let context = NewContext {};
    Template::render("new_page", &context)
}

#[post("/", data = "<form>")]
pub fn new_page_post(form: Form<NewForm>, state: State<'_, AppState>) -> Result<Redirect, Status> {
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
pub struct EditForm {
    content: String,
}

#[get("/<file..>")]
pub fn edit_page(file: PathBuf, state: State<'_, AppState>) -> Result<Template, Status> {
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
pub fn edit_page_post(
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
pub async fn upload_image(
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
