# vibe-backend

You can make an endpoint public by writing “anyone can …”; public/auth flags are optional.

**Describe the backend (server side) of your app in plain words, and vibe-backend turns it into a working web API.**

## Who is this for?

- People with an app idea who don't know programming.
- Designers, product people and students who want a real backend without writing code.
- Developers who would rather write a short description than lots of boilerplate.

You write simple text files (Markdown) that say what your API should do, in English,
Chinese or another language. vibe-backend uses an AI model, with **your own** AI key,
to turn them into a clear, checked file. Then it runs your API from that file.
The AI is only used when you build, never while your API is running.

## Quick start (3 steps)

**1. Install.** On macOS or Linux, download this project and run the installer
(you need the [GitHub CLI](https://cli.github.com/) installed and signed in;
the installer uses it to check that the download is genuine):

```sh
git clone https://github.com/vibe-backend/sdk.git
cd sdk
./scripts/vibe-install.sh install --version v1.0.0-rc.8
```

This installs the `vibe` command. On Windows, download the zip from
[Releases](https://github.com/vibe-backend/sdk/releases). More options are in the
[installation guide](docs/install.md).

**2. Tell `vibe` what you want.** In a new, empty folder, run:

```sh
vibe
```

The first time, `vibe` asks for your AI settings and key once (your key is hidden
as you type). It saves them only on your computer in your personal settings folder
and reuses them for your other projects. Then type what you want, for example
*"make a hello world API"*. `vibe` shows you the files it wants to create and asks
before writing anything.

**3. Build and run.**

```sh
vibe build
vibe run
```

`vibe build` turns your text into a checked file. `vibe run` starts your API on your
computer (at `http://127.0.0.1:8080`). Change your text any time, then run both again.

## What a description looks like

A file that describes one API endpoint can be as small as this:

```markdown
# Hello World API

Create a public endpoint that anyone can call without logging in.
It always answers with the JSON message {"message": "hello world"}.
```

You don't have to write these files yourself: `vibe` creates them from your request,
and you can then edit them like any text document.

## See a full example

[vibe-backend/sample-hello-world](https://github.com/vibe-backend/sample-hello-world)
is a small, complete project: a "hello" message and a counter, with a backend and a
simple web page.

## Learn more

- [Installation guide](docs/install.md): all install options, upgrades and removal.
- [Technical details](docs/README-details.md): commands and options, deployment
  (`vibe-runtime`), runtime features, security, signing, CI/CD.
- [Runtime configuration](docs/runtime-config.md): databases, environment variables
  and production settings.
- [Specification](docs/spec-v1.8.md) and [project status](docs/status.md).
- [Changelog](CHANGELOG.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
