use crate::config::{Config, User};
use crate::utils::*;
use crate::wiki::WikiRequest;

use async_std::fs;
use async_std::path::{Path, PathBuf};

use rocket::data::{Data, ToByteUnit};
use rocket::http::{ContentType, Cookie, CookieJar, Status};
use rocket::request::{self, FlashMessage, Form, FromRequest, Request};
use rocket::response::{Flash, Redirect};
use rocket::tokio::sync::{mpsc, oneshot};
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

pub struct WebappState {
    tx: mpsc::Sender<WikiRequest>,
}

impl WebappState {
    pub fn new(tx: mpsc::Sender<WikiRequest>) -> Self {
        WebappState { tx }
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
pub async fn new_page_post(
    form: Form<NewForm>,
    user: User,
    state: State<'_, WebappState>,
) -> Result<Redirect, Status> {
    // TODO check for legal characters in path
    let form_file = form.file.replace(" ", "_");
    let file = Path::new(&form_file);

    let (tx, rx) = oneshot::channel();
    state
        .tx
        .send(WikiRequest::CreateFile {
            user,
            file: file.to_path_buf().into_boxed_path(),
            content: form.content.clone(),
            respond: tx,
        })
        .await
        .map_err(|_| Status::InternalServerError)?;

    if !rx.await.map_err(|_| Status::InternalServerError)?.is_ok() {
        return Err(Status::InternalServerError);
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
    file: std::path::PathBuf,
    content: String,
}

#[derive(FromForm)]
pub struct EditForm {
    content: String,
}

#[get("/<file..>")]
pub async fn edit_page(
    file: std::path::PathBuf,
    _user: User,
    config: State<'_, Config>,
) -> Result<Template, Status> {
    if !config.can_edit(&PathBuf::from(&file)).await {
        return Err(Status::NotFound);
    }
    let path = Path::new(&config.path).join("src").join(&file);
    let content = fs::read_to_string(&path)
        .await
        .map_err(log_warn)
        .map_err(|_| Status::NotFound)?;
    let context = EditContext { file, content };
    Ok(Template::render("edit_page", &context))
}

#[post("/<file..>", data = "<form>")]
pub async fn edit_page_post(
    file: std::path::PathBuf,
    form: Form<EditForm>,
    user: User,
    state: State<'_, WebappState>,
) -> Result<Redirect, Status> {
    let (tx, rx) = oneshot::channel();
    state
        .tx
        .send(WikiRequest::EditFile {
            user,
            file: PathBuf::from(file.to_path_buf()).into_boxed_path(),
            content: form.content.clone(),
            respond: tx,
        })
        .await
        .map_err(|_| Status::InternalServerError)?;

    if !rx.await.map_err(|_| Status::InternalServerError)?.is_ok() {
        return Err(Status::InternalServerError);
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
    config: State<'_, Config>,
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

    let file_path = Path::new(&config.path)
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
