# [WIP] mdwiki

"Wrapper" for [mdBook](https://github.com/rust-lang/mdBook) that adds support for creating and editing files from the browser. mdwiki serves a local directory containing an mdbook instance and a git repo, and committing changes to the repo. mdwiki tries to be "minimally invasive" in that the directory will still work as an mdbook instance, but it does expect a certain file/directory structure.

### Try it out

Run:

```bash
RUST_LOG=info MDWIKI_PATH=/tmp/mdwiki cargo run
```

... and visit http://localhost:8000

### TODO

- Move/delete files
- Customization
- Upload images
- Push commits to remote
- Auth & commit as different users
- Review changes to file
- Page templates
