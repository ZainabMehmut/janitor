# Multi-host deployment

The playbook supports splitting the worker onto its own host, talking to
the rest of the stack (runner, site, postgres, redis, differ, git_store,
publish, archive, auto_upload, caddy, bzr_store) on a separate host over
the network instead of localhost. Single-host (one host running
everything) is the default and needs none of this - it's what you get
by putting the same host in both `[control]` and `[worker]` in
`inventory.ini`.

## Inventory

`ansible/inventory.ini` has two groups, `[control]` and `[worker]`.
Single-host: one host in both. Two-host: the control-plane host in
`[control]`, the worker-only host in `[worker]`.

## What group_vars/control.yml and worker.yml actually do

Both files only take effect for a genuinely split host - every variable
in them is guarded with a `group_names` check (`'worker' not in
group_names` in `control.yml`, `'control' not in group_names` in
`worker.yml`) that falls through to `group_vars/all.yml`'s single-host
defaults otherwise. This guard exists because Ansible resolves
same-depth `group_vars` files alphabetically when a host is in more than
one group - `control.yml` before `worker.yml` - so without it,
`worker.yml`'s plain (unguarded) values would win outright even during
the single-host default's `control` play, and `janitor_services` would
resolve to `["worker"]` only on every host, dropping `site`,
`git_store`, `differ`, and the rest.

- `janitor_services` - which quadlet units get deployed to this host.
  Control gets everything except `worker`; worker gets only `worker`
  (unless it's also in `[control]`, the single-host case, where it gets
  both lists).
- `deploy_janitor_conf` (`worker.yml` only) - `false` for a genuinely
  worker-only host, since nothing there reads `janitor_home/data/*` or
  `janitor.conf` (see `roles/janitor_config`). `true` whenever the host
  is also in `[control]`.
- Cross-host address vars - `git_store_bind_address` /
  `git_store_public_address`, `runner_bind_address`,
  `runner_base_url_host`, `worker_listen_address` /
  `worker_external_address`. Only meaningful when `[control]` and
  `[worker]` are different hosts: each defaults to the literal string
  `CHANGEME` and must be filled in with the real, reachable address
  before a genuine split deploy. Resolve automatically to
  `127.0.0.1`/`localhost` for the single-host default.

## Gitea token recovery on a split host

`roles/gitea/tasks/main.yml`'s API call to create the janitor bot's
access token only returns the token's value on the run that actually
creates it - a re-run only sees the token's name/scopes, not its value,
so the role's own idempotency can't recover it. If a run fails after
token creation but before that value gets recorded, the fact falls back
to the literal string `CHANGEME-see-docs-multi-host-deployment` and the
next task that needs the real token will fail. Recovery: drop the token
from Gitea's admin UI, then delete `gitea_home/data` too so
`INSTALL_LOCK` is cleared and the role re-provisions cleanly from
scratch.

## Optional gitea/auto_upload_target roles

Both are off unless explicitly turned on - `roles/gitea` via
`enable_gitea: false` in `group_vars/all.yml`, `roles/auto_upload_target`
via leaving `auto_upload_dput_host` unset (its default - `auto_upload`
itself always runs, it just has nowhere to upload to). Neither is tied
to the single-host/multi-host split - see
[optional services](optional-services.md) for what each provisions and
when you'd want it.

## Testing locally with Vagrant

`deploy/Vagrantfile.multi-host` stands up two local VMs (`control`,
`worker`) running the same playbook as a real split deployment, for
validating a change without needing real hosts.

```
vagrant up                              # fast path: pull pre-built images from ghcr.io
JANITOR_USE_PREBUILT=false vagrant up   # slow path: build all images from source (several hours)
vagrant provision                       # re-run both playbooks without recreating either VM
vagrant ssh control / vagrant ssh worker
vagrant destroy
```

Environment variables (all optional):

- `JANITOR_USE_PREBUILT` (default `true`)
- `JANITOR_VM_CPUS` (default `4`) / `JANITOR_VM_MEMORY` (default `4096`,
  or `8192` when building from source)
- `JANITOR_CONTROL_IP` (default `192.168.60.11`) /
  `JANITOR_WORKER_IP` (default `192.168.60.12`)
- `JANITOR_REPO` / `JANITOR_BRANCH` (default upstream `main`)
- `JANITOR_STORAGE_POOL` (default `janitor-multivm-ssd`)
