#![feature(proc_macro_hygiene, decl_macro, async_closure)]

#[macro_use]
mod utils;
mod config;
mod webapp;
mod wiki;

#[macro_use]
extern crate rocket;

#[macro_use]
extern crate log;

use config::Config;
use webapp::WebappState;
use wiki::WikiState;

use rocket::fairing::AdHoc;
use rocket::figment::Figment;
use rocket::futures::join;
use rocket::tokio::task;
use rocket_contrib::helmet::SpaceHelmet;
use rocket_contrib::templates::Template;

fn rocket(state: WebappState) -> rocket::Rocket {
    use webapp::*;

    let figment = Figment::from(rocket::Config::default()).merge(Config::figment());

    rocket::custom(figment)
        .attach(AdHoc::config::<Config>())
        .attach(Template::fairing())
        .attach(SpaceHelmet::default())
        .manage(state)
        .mount(
            "/",
            routes![
                index,
                book_files,
                new_page,
                new_page_post,
                edit_page,
                edit_page_post,
                upload_image,
                mdwiki_script,
                login,
                login_post,
                logout,
            ],
        )
}

#[rocket::main]
async fn main() {
    env_logger::init_from_env("LOG_LEVEL");

    let (wiki_state, webapp_state) = WikiState::new();

    wiki_state.setup().await.unwrap();

    let wiki = task::spawn(async { wiki_state.serve().await });

    join!(wiki, rocket(webapp_state).launch()).1.unwrap();
}

#[cfg(test)]
mod test {
    use super::*;

    use std::future::Future;

    use rocket::futures::executor::block_on;
    use rocket::http::{ContentType, Status};
    use rocket::local::asynchronous::Client;

    use figment::Jail;

    const TEST_CONFIG: &str = r#"
[debug]
secret_key = "DEBUGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGG"

[[debug.users]]
username = "user"
password = "password"
"#;

    fn run_test<Fut>(setup_jail: Option<fn(&mut Jail)>, test: impl FnOnce(Client) -> Fut)
    where
        Fut: Future<Output = Result<(), figment::Error>>,
    {
        Jail::expect_with(|jail| {
            block_on(async {
                let book_path = jail.directory().join("mdwiki-test-dir");
                jail.create_file("mdwiki.toml", TEST_CONFIG).unwrap();
                jail.set_env("MDWIKI_PATH", book_path.to_str().unwrap());

                if let Some(setup_jail) = setup_jail {
                    setup_jail(jail);
                }

                let (wiki_state, webapp_state) = WikiState::new();

                wiki_state.setup().await.unwrap();

                let rocket = rocket(webapp_state);

                let wiki = task::spawn(async { wiki_state.serve().await });

                let client = Client::tracked(rocket)
                    .await
                    .expect("valid rocket instance");

                join!(wiki, test(client)).1
            })
        });
    }

    #[rocket::async_test]
    async fn bootstrap_wiki() {
        run_test(None, async move |client: Client| {
            assert_eq!(
                client.get("/").dispatch().await.status(),
                Status::PermanentRedirect
            );
            assert_eq!(
                client.get("/SUMMARY.html").dispatch().await.status(),
                Status::Ok
            );

            let response = client.get("/index.html").dispatch().await;
            assert_eq!(response.status(), Status::Ok);
            assert!(response
                .into_string()
                .await
                .unwrap()
                .contains(r#"// mdwiki theme override script to add "edit" and "new" buttons"#));

            Ok(())
        });
    }

    #[rocket::async_test]
    async fn login() {
        run_test(None, async move |client: Client| {
            let response = client
                .post("/login")
                .header(ContentType::Form)
                .body("username=user&password=password")
                .dispatch()
                .await;

            assert_eq!(response.status(), Status::SeeOther);
            assert_eq!(response.headers().get_one("location"), Some("/"));

            Ok(())
        });
    }

    #[rocket::async_test]
    async fn new_page() {
        run_test(None, async move |client: Client| {
            client
                .post("/login")
                .header(ContentType::Form)
                .body("username=user&password=password")
                .dispatch()
                .await;

            let response = client
                .post("/new")
                .header(ContentType::Form)
                .body("file=newfile.md&content=NEWPAGE")
                .dispatch()
                .await;

            assert_eq!(response.status(), Status::SeeOther);
            assert_eq!(
                response.headers().get_one("location"),
                Some("/newfile.html")
            );

            let response = client.get("/newfile.html").dispatch().await;
            assert_eq!(response.status(), Status::Ok);
            assert!(response.into_string().await.unwrap().contains("NEWPAGE"));

            Ok(())
        });
    }

    #[rocket::async_test]
    async fn new_page_with_dirs() {
        run_test(None, async move |client: Client| {
            client
                .post("/login")
                .header(ContentType::Form)
                .body("username=user&password=password")
                .dispatch()
                .await;

            let response = client
                .post("/new")
                .header(ContentType::Form)
                .body("file=newdir/newfile.md&content=NEWPAGE")
                .dispatch()
                .await;

            assert_eq!(response.status(), Status::SeeOther);
            assert_eq!(
                response.headers().get_one("location"),
                Some("/newdir/newfile.html")
            );

            assert_eq!(
                client.get("/newdir/").dispatch().await.status(),
                Status::PermanentRedirect
            );
            assert_eq!(
                client.get("/newdir/index.html").dispatch().await.status(),
                Status::Ok
            );
            assert_eq!(
                client.get("/newdir/newfile.html").dispatch().await.status(),
                Status::Ok
            );

            Ok(())
        });
    }

    #[rocket::async_test]
    async fn edit_page() {
        run_test(None, async move |client: Client| {
            client
                .post("/login")
                .header(ContentType::Form)
                .body("username=user&password=password")
                .dispatch()
                .await;

            let response = client
                .post("/edit/README.md")
                .header(ContentType::Form)
                .body("content=EDITEDCONTENT")
                .dispatch()
                .await;

            assert_eq!(response.status(), Status::SeeOther);
            assert_eq!(response.headers().get_one("location"), Some("/"));

            let response = client.get("/index.html").dispatch().await;
            assert_eq!(response.status(), Status::Ok);
            assert!(response
                .into_string()
                .await
                .unwrap()
                .contains("EDITEDCONTENT"));

            Ok(())
        })
    }

    #[rocket::async_test]
    async fn anonymous_users_not_allowed() {
        run_test(
            Some(|jail: &mut Jail| {
                jail.set_env("MDWIKI_ALLOW_ANONYMOUS", "false");
            }),
            async move |client: Client| {
                assert_eq!(
                    client.get("/index.html").dispatch().await.status(),
                    Status::SeeOther
                );

                client
                    .post("/login")
                    .header(ContentType::Form)
                    .body("username=user&password=password")
                    .dispatch()
                    .await;

                assert_eq!(
                    client.get("/index.html").dispatch().await.status(),
                    Status::Ok
                );

                Ok(())
            },
        )
    }
}
