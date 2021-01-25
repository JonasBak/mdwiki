use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{Config, User};
use crate::utils::*;
use crate::wiki::AppState;

use rocket::data::{Data, ToByteUnit};
use rocket::http::{ContentType, Cookie, CookieJar, Status};
use rocket::request::{self, FlashMessage, Form, FromRequest, Request};
use rocket::response::{Flash, Redirect};
use rocket::State;

use rocket_contrib::templates::Template;

use serde::Serialize;

const MDWIKI_AUTH_COOKIE: &str = "mdwiki_auth";

#[rocket::async_trait]
impl<'a, 'r> FromRequest<'a, 'r> for User {
    type Error = ();

    async fn from_request(req: &'a Request<'r>) -> request::Outcome<Self, Self::Error> {
        let username_cookie = if let Some(username) = req.cookies().get_private(MDWIKI_AUTH_COOKIE)
        {
            username
        } else {
            return request::Outcome::Forward(());
        };

        let user = if let Some(user) = try_outcome!(req.guard::<State<'r, Config>>().await)
            .users
            .iter()
            .find(|user| user.username == username_cookie.value())
        {
            user.clone()
        } else {
            return request::Outcome::Failure((Status::BadRequest, ()));
        };

        request::Outcome::Success(user)
    }
}

#[derive(Serialize)]
struct LoginContext {
    status_message: Option<String>,
    user: Option<String>,
}

#[derive(FromForm)]
pub struct LoginForm {
    username: String,
    password: String,
}

#[get("/")]
pub fn login(flash: Option<FlashMessage>, user: Option<User>) -> Template {
    let context = LoginContext {
        status_message: flash.map(|f| f.msg().to_string()),
        user: user.map(|user| user.username),
    };
    Template::render("login", &context)
}

#[post("/", data = "<form>")]
pub fn login_post(
    form: Form<LoginForm>,
    config: State<'_, Config>,
    cookies: &CookieJar<'_>,
) -> Result<Redirect, Flash<Redirect>> {
    let user = if let Some(user) = config
        .users
        .iter()
        .find(|user| user.username == form.username)
    {
        user
    } else {
        return Err(Flash::error(
            Redirect::to("/login"),
            "Invalid username/password.",
        ));
    };
    if user.password == form.password {
        let mut cookie = Cookie::new(MDWIKI_AUTH_COOKIE, user.username.clone());
        cookie.set_http_only(false);
        cookies.add_private(cookie);
        return Ok(Redirect::to("/"));
    }
    Err(Flash::error(
        Redirect::to("/login"),
        "Invalid username/password.",
    ))
}

#[get("/")]
pub fn logout(cookies: &CookieJar<'_>) -> Redirect {
    cookies.remove_private(Cookie::named(MDWIKI_AUTH_COOKIE));
    Redirect::to("/")
}

#[derive(Serialize)]
struct ScriptContext {
    logged_in: bool,
}

#[get("/")]
pub fn mdwiki_script(user: Option<User>) -> Template {
    let context = ScriptContext {
        logged_in: user.is_some(),
    };
    Template::render("mdwiki_script", &context)
}

#[derive(Serialize)]
struct NewContext {}

#[derive(FromForm)]
pub struct NewForm {
    file: String,
    content: String,
}

#[get("/")]
pub fn new_page(_user: User) -> Template {
    let context = NewContext {};
    Template::render("new_page", &context)
}

#[post("/", data = "<form>")]
pub fn new_page_post(
    form: Form<NewForm>,
    user: User,
    state: State<'_, AppState>,
) -> Result<Redirect, Status> {
    // TODO check for legal characters in path
    let form_file = form.file.replace(" ", "_");
    let file = Path::new(&form_file);
    if !state.can_create(&file) {
        return Err(Status::BadRequest);
    }

    {
        let _ = state.dir_lock.lock().unwrap();

        let path = Path::new(&state.config.path).join("src").join(&file);

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
            let index = Path::new(&state.config.path)
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
            .on_created(&user, &file)
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
pub fn edit_page(
    file: PathBuf,
    _user: User,
    state: State<'_, AppState>,
) -> Result<Template, Status> {
    if !state.can_edit(&file) {
        return Err(Status::NotFound);
    }
    let path = Path::new(&state.config.path).join("src").join(&file);
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
    user: User,
    state: State<'_, AppState>,
) -> Result<Redirect, Status> {
    if !state.can_edit(&file) {
        return Err(Status::NotFound);
    }

    {
        let _ = state.dir_lock.lock().unwrap();

        let path = Path::new(&state.config.path).join("src").join(&file);
        fs::write(path, &form.content)
            .map_err(log_warn)
            .map_err(|_| Status::InternalServerError)?;

        state
            .on_edited(&user, &file)
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
    _user: User,
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

    let file_path = Path::new(&state.config.path)
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
