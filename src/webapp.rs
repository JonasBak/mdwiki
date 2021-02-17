use crate::config::{Config, User};
use crate::utils::*;
use crate::wiki::WikiRequest;

use async_std::fs;
use async_std::path::{Path, PathBuf};

use rocket::data::{Data, ToByteUnit};
use rocket::http::{ContentType, Cookie, CookieJar, Status};
use rocket::request::{self, FlashMessage, Form, FromRequest, Request};
use rocket::response::NamedFile;
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
    message: Option<String>,
    user: Option<String>,
}

#[derive(FromForm)]
pub struct LoginForm {
    username: String,
    password: String,
}

#[get("/login")]
pub fn login(message: Option<FlashMessage>, user: Option<User>) -> Template {
    let context = LoginContext {
        message: message.map(|f| f.msg().to_string()),
        user: user.map(|user| user.username),
    };
    Template::render("login", &context)
}

#[post("/login", data = "<form>")]
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

#[get("/logout")]
pub fn logout(cookies: &CookieJar<'_>) -> Redirect {
    cookies.remove_private(Cookie::named(MDWIKI_AUTH_COOKIE));
    Redirect::to("/")
}

#[derive(Serialize)]
struct ScriptContext {
    logged_in: bool,
}

#[get("/mdwiki_script.js")]
pub fn mdwiki_script(user: Option<User>) -> Template {
    let context = ScriptContext {
        logged_in: user.is_some(),
    };
    Template::render("mdwiki_script", &context)
}

#[derive(Serialize)]
struct NewContext {
    file: String,
    content: String,
    message: Option<String>,
}

#[derive(FromForm)]
pub struct NewForm {
    file: String,
    content: String,
}

#[get("/new")]
pub fn new_page(message: Option<FlashMessage>, _user: User) -> Template {
    let context = NewContext {
        file: "".to_string(),
        content: "".to_string(),
        message: message.map(|f| f.msg().to_string()),
    };
    Template::render("new_page", &context)
}

#[post("/new", data = "<form>")]
pub async fn new_page_post(
    form: Form<NewForm>,
    user: User,
    state: State<'_, WebappState>,
) -> Result<Redirect, Template> {
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
        .map_err(log_warn)
        .map_err(|_| "")
        .unwrap();

    let res = rx.await.map_err(log_warn).unwrap();
    if !res.is_ok() {
        let context = NewContext {
            file: form.file.clone(),
            content: form.content.clone(),
            message: Some(
                res.msg()
                    .cloned()
                    .unwrap_or("Something went wrong :(".to_string()),
            ),
        };
        return Err(Template::render("new_page", &context));
    }

    let html_file = Path::new(&form.file).with_extension("html");
    return Ok(Redirect::to(format!(
        "/{}",
        html_file
            .to_str()
            .unwrap()
            .replace("README.html", "")
            .to_string()
    )));
}

#[derive(Serialize)]
struct EditContext {
    file: std::path::PathBuf,
    content: String,
    message: Option<String>,
}

#[derive(FromForm)]
pub struct EditForm {
    content: String,
}

#[get("/edit/<file..>")]
pub async fn edit_page(
    file: std::path::PathBuf,
    message: Option<FlashMessage<'_, '_>>,
    _user: User,
    config: State<'_, Config>,
) -> Result<Template, Option<Flash<Redirect>>> {
    if !config.can_edit(&PathBuf::from(&file)).await.is_ok() {
        return Err(None);
    }
    let path = Path::new(&config.path).join("src").join(&file);
    let content = fs::read_to_string(&path)
        .await
        .map_err(log_warn)
        .map_err(|_| None)?;
    let context = EditContext {
        file,
        content,
        message: message.map(|f| f.msg().to_string()),
    };
    Ok(Template::render("edit_page", &context))
}

#[post("/edit/<file..>", data = "<form>")]
pub async fn edit_page_post(
    file: std::path::PathBuf,
    form: Form<EditForm>,
    user: User,
    state: State<'_, WebappState>,
) -> Result<Redirect, Template> {
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
        .map_err(log_warn)
        .map_err(|_| "")
        .unwrap();

    let res = rx.await.map_err(log_warn).unwrap();
    if !res.is_ok() {
        let context = EditContext {
            file,
            content: form.content.clone(),
            message: Some(
                res.msg()
                    .cloned()
                    .unwrap_or("Something went wrong :(".to_string()),
            ),
        };
        return Err(Template::render("edit_page", &context));
    }

    let html_file = file.with_extension("html");
    return Ok(Redirect::to(format!(
        "/{}",
        html_file
            .to_str()
            .unwrap()
            .replace("README.html", "")
            .to_string()
    )));
}

#[post("/upload/image", data = "<data>")]
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

    let file_path = Path::new(&config.tmp_upload_path)
        .join(&filename)
        .with_extension(&extension);

    data.open(8_u8.mebibytes())
        .stream_to_file(file_path)
        .await
        .map_err(log_warn)
        .map_err(|_| ())?;

    Ok(format!("/images/{}.{}", filename, extension))
}

#[get("/", rank = 10)]
pub async fn index() -> Redirect {
    Redirect::permanent("/index.html")
}

#[get("/<path..>", rank = 10)]
pub async fn book_files(
    path: std::path::PathBuf,
    user: Option<User>,
    config: State<'_, Config>,
) -> Result<Option<NamedFile>, Redirect> {
    const SAFE_PREFIXES: &[&'static str] = &["css", "FontAwesome", "favicon.svg"];

    if !config.allow_anonymous
        && user.is_none()
        && SAFE_PREFIXES
            .iter()
            .find(|prefix| path.starts_with(prefix))
            .is_none()
    {
        return Err(Redirect::to(uri!(login)));
    }

    let full_path = Path::new(&config.path).join(&config.book_path).join(&path);

    if full_path.is_dir().await {
        return Err(Redirect::permanent(format!(
            "/{}",
            path.join("index.html").to_str().unwrap()
        )));
    }

    Ok(NamedFile::open(full_path).await.ok())
}
