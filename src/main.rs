#![feature(proc_macro_hygiene, decl_macro)]

mod config;
mod routes;
mod utils;
mod wiki;

#[macro_use]
extern crate rocket;

#[macro_use]
extern crate log;

use std::path::Path;

use config::Config;
use wiki::AppState;

use rocket::fairing::AdHoc;
use rocket::figment::Figment;
use rocket_contrib::helmet::SpaceHelmet;
use rocket_contrib::serve::StaticFiles;
use rocket_contrib::templates::Template;

fn rocket(state: AppState, static_path: &Path) -> rocket::Rocket {
    use routes::*;

    let figment = Figment::from(rocket::Config::default()).merge(Config::figment());

    rocket::custom(figment)
        .attach(AdHoc::config::<Config>())
        .attach(Template::fairing())
        .attach(SpaceHelmet::default())
        .manage(state)
        .mount("/new", routes![new_page, new_page_post])
        .mount("/edit", routes![edit_page, edit_page_post])
        .mount("/upload", routes![upload_image,])
        .mount("/login", routes![login, login_post])
        .mount("/logout", routes![logout,])
        .mount("/mdwiki_script.js", routes![mdwiki_script,])
        .mount("/", StaticFiles::from(static_path))
}

#[rocket::main]
async fn main() {
    env_logger::init_from_env("LOG_LEVEL");

    let state = AppState::new();

    let build_path = state.setup().unwrap();

    rocket(state, &*build_path).launch().await.unwrap();
}

#[cfg(test)]
mod test {
    use std::path::Path;

    use rocket::http::{ContentType, Status};
    use rocket::local::blocking::Client;

    use figment::Jail;

    const TEST_CONFIG: &str = r#"
[debug]
secret_key = "DEBUGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGG"

[[debug.users]]
username = "user"
password = "password"
"#;

    fn get_rocket_instance(jail: &mut Jail) -> (rocket::Rocket, Box<Path>) {
        use super::*;

        let book_path = jail.directory().join("mdwiki-test-dir");

        jail.create_file("mdwiki.toml", TEST_CONFIG).unwrap();

        jail.set_env("MDWIKI_PATH", book_path.to_str().unwrap());

        let state = AppState::new();

        let build_path = state.setup().unwrap();

        (rocket(state, &*build_path), book_path.into_boxed_path())
    }

    #[test]
    fn bootstrap_wiki() {
        Jail::expect_with(|jail| {
            let (rocket, _book_path) = get_rocket_instance(jail);

            let client = Client::tracked(rocket).expect("valid rocket instance");

            assert_eq!(client.get("/index.html").dispatch().status(), Status::Ok);
            assert_eq!(client.get("/SUMMARY.html").dispatch().status(), Status::Ok);

            let response = client.get("/").dispatch();
            assert_eq!(response.status(), Status::Ok);
            assert!(response
                .into_string()
                .unwrap()
                .contains(r#"// mdwiki theme override script to add "edit" and "new" buttons"#));

            Ok(())
        });
    }

    #[test]
    fn login() {
        Jail::expect_with(|jail| {
            let (rocket, _book_path) = get_rocket_instance(jail);

            let client = Client::tracked(rocket).expect("valid rocket instance");

            let response = client
                .post("/login")
                .header(ContentType::Form)
                .body("username=user&password=password")
                .dispatch();

            assert_eq!(response.status(), Status::SeeOther);
            assert_eq!(response.headers().get_one("location"), Some("/"));

            Ok(())
        });
    }

    #[test]
    fn new_page() {
        Jail::expect_with(|jail| {
            let (rocket, _book_path) = get_rocket_instance(jail);

            let client = Client::tracked(rocket).expect("valid rocket instance");

            client
                .post("/login")
                .header(ContentType::Form)
                .body("username=user&password=password")
                .dispatch();

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

            Ok(())
        });
    }

    #[test]
    fn new_page_with_dirs() {
        Jail::expect_with(|jail| {
            let (rocket, _book_path) = get_rocket_instance(jail);

            let client = Client::tracked(rocket).expect("valid rocket instance");

            client
                .post("/login")
                .header(ContentType::Form)
                .body("username=user&password=password")
                .dispatch();

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

            Ok(())
        })
    }

    #[test]
    fn edit_page() {
        Jail::expect_with(|jail| {
            let (rocket, _book_path) = get_rocket_instance(jail);

            let client = Client::tracked(rocket).expect("valid rocket instance");

            client
                .post("/login")
                .header(ContentType::Form)
                .body("username=user&password=password")
                .dispatch();

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

            Ok(())
        })
    }
}
