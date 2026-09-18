# Optional services

[`deploy/`](../deploy/README.md)'s playbook deploys the janitor stack
itself only - nothing it talks to (GitHub, a real APT archive) is stood
up for you, and it's assumed to already exist. Two roles cover the gap
for local testing and proof-of-concept work: a real Gitea instance for
exercising the Gitea forge plugin, and a real (if local-only) upload
target for `auto_upload`. Both are disabled by default and opt-in, the
same "omit to disable" convention `group_vars/all.yml` already uses for
OAuth - the default single-host and multi-host deployments described
elsewhere are unaffected either way.

## Gitea

`roles/gitea` stands up a real Gitea instance the way the forge plugin's
own end-to-end proof needed one in the first place: a real account with
a real access token, wired into breezy's `authentication.conf` for
janitor's runner/publish to actually push branches and open merge
proposals against.

Enable it with `enable_gitea: true` in `group_vars/all.yml` (or
`-e enable_gitea=true`), on whichever host is in `[control]` - it runs
before `janitor_config` so the token it generates is available for
`authentication.conf` on the same deploy.

Variables (all in `group_vars/all.yml`, see the comments there for full
detail):

| Variable | Meaning |
|---|---|
| `gitea_domain` | Hostname janitor's `authentication.conf` and Gitea's own `ROOT_URL` both use. Needs to actually resolve from wherever the janitor services run - `/etc/hosts` or real DNS, this role doesn't manage that. |
| `gitea_http_port` / `gitea_ssh_port` | Gitea's web and SSH ports. |
| `gitea_bot_user` / `gitea_bot_password` | The account janitor pushes as. The password is only used the first time the account is created - Gitea has no idempotent way to read it back afterward. |

What it provisions: a `gitea/gitea` container (rootless quadlet, same
pattern as every janitor service), an `app.ini` with registration
disabled and the install lock already set (matches how the original
proof instance was configured by hand), the bot account, and an access
token added as a `[gitea-local]` stanza in `authentication.conf`.

**Token recovery isn't automatic.** Gitea never exposes a token's value
again after creation - if this role's own state is lost (the host
rebuilt, `janitor_home/authentication.conf` deleted) it can't recover the
old token from the running Gitea instance either, it can only mint a new
one under a different token name. `gitea admin user generate-access-token`
errors on a repeated run with the same `--token-name`, so re-running this
role against an already-provisioned Gitea instance leaves
`authentication.conf`'s existing entry alone rather than silently
breaking it - if you actually need a fresh token, delete the old one from
Gitea's admin UI first.

Not meant as a production forge - it's a real server, not a mock, but
this role's own defaults (a fixed bot password var, no TLS, no backup)
are proof-of-concept configuration, not something to point a real
deployment's traffic at.

## `auto_upload`'s local upload target

`auto_upload` signs (`debsign`) and uploads (`dput`) every Debian build
result to whatever `--dput-host`/`--debsign-keyid` it's given - neither
has a default, and until now neither the Dockerfile nor the playbook
wired them to anything, so a stock deployment's `auto_upload` never
actually uploads anywhere (nor does its container image even have `dput`
installed).

`roles/auto_upload_target` gives it something real to upload to for
testing: a local incoming directory plus a signing key, both on the same
host `auto_upload` runs on - not a real archive, but real enough that
`dput`/`debsign` actually run against something instead of failing
immediately on a missing target.

Enable it by setting `auto_upload_dput_host` in `group_vars/all.yml`
(any name - it becomes the section header in the generated `dput.cf`,
matched against the `--dput-host` flag). Point it at a real archive's
`dput.cf` entry instead for an actual production upload target; this
role's local-directory setup is a proof-of-concept substitute for that,
not a replacement.

What it provisions: `janitor_home/dput-incoming/` (what `dput`'s `method
= local` copies uploads into), a `janitor_home/gnupg` keyring with a
freshly generated signing key (batch-generated, no passphrase - it only
needs to exist for `debsign` to use, not to be a trusted real identity),
and `janitor_home/dput.cf` bind-mounted into the `auto_upload` container
at `/etc/dput.cf` (where `dput` reads it by default - no flag or env var
needed for this part).

Requires `Dockerfile_auto_upload`'s final image to actually have `dput`
installed - true once this same change lands there, not yet true of any
already-published `ghcr.io` image, so a deployment using
`use_prebuilt_images: true` won't see this take effect until upstream
publishes a rebuilt image with that Dockerfile change in it. A source
build (`use_prebuilt_images: false`) picks it up immediately.
