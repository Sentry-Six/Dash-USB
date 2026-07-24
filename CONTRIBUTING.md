# Contributing to Dash USB

Thanks for your interest in improving Dash-USB. This guide covers how to
contribute and the licensing terms that apply to contributions.

## Contributor License Agreement (required)

This project is **source-available, not open source**: the original code is
licensed under the [PolyForm Noncommercial License 1.0.0](LICENSE), and the
Maintainers also offer it under separate commercial licenses. To keep that dual
licensing possible, every contributor must agree to the
[Contributor License Agreement (CLA)](CLA.md) before their pull request can be
merged.

You only sign once, and it covers all your future contributions.

**How signing works:**

1. Open your pull request as usual.
2. The **CLA Assistant** bot comments on the PR with a link to the CLA.
3. If you have not signed before, reply to the bot on the PR with exactly:

   > I have read the CLA Document and I hereby sign the CLA

4. The bot records your signature and the PR's CLA check turns green. Maintainers
   can then merge.

If you are contributing on behalf of a company, or your employer owns IP you
create, make sure you have authorization (see CLA section 4) before signing.

## Licensing of contributions

- Contributions to the Rust crates and other original project files are
  licensed under the PolyForm Noncommercial License 1.0.0, and — per the CLA —
  may also be relicensed by the Maintainers under commercial terms.
- A small set of shell scripts derive from
  [TeslaUSB](https://github.com/marcone/teslausb) and remain under the **MIT
  License**. These files are listed in [NOTICE](NOTICE). Changes to those files
  stay under MIT.

If you are unsure which license applies to a file you are editing, check
[NOTICE](NOTICE) or ask in your pull request.

## Development

This is a Cargo workspace. Common commands from the repo root:

```bash
cargo build            # build all crates
cargo test             # run tests
cargo fmt --all        # format
cargo clippy --all-targets --all-features   # lint
```

The web UI lives in `web/` and uses its own toolchain (see `web/package.json`).

## Pull request guidelines

- Keep changes focused; one logical change per PR.
- Run `cargo fmt` and `cargo clippy` before submitting.
- Describe what the change does and why. Link any related issue.
- For changes to MIT-derived scripts (see [NOTICE](NOTICE)), note it in the PR
  description so reviewers can preserve the license boundary.

## Reporting bugs and requesting features

Open a GitHub issue with clear steps to reproduce (for bugs) or a description of
the use case (for features). For security-sensitive reports, contact the
Maintainers directly rather than opening a public issue.
