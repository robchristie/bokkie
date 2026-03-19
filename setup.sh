#• Use a dedicated bokkie service user on both hosts, and keep the human workflow separate.
#
#  Recommended Layout
#
#  On both the app server and dev box:
#
#  user: bokkie
#  group: bokkie
#
#  /home/bokkie/.codex/          Codex auth/config/skills
#  /home/bokkie/.ssh/            executor SSH keys
#  /srv/bokkie/                  git checkout of this repo
#  /srv/bokkie/.bokkie/          run artifacts
#  /srv/bokkie/run/              local runtime state you want out of git
#  /srv/bokkie/.worker-cache/    mirror cache
#  /srv/bokkie/.worker-worktrees/ worktrees
#
#  I would keep the repo itself at /srv/bokkie and run Bokkie from there. That matches how the
#  current runtime expects a stable checkout and artifact root.
#
#  1. Create The User
#
#  On both machines:

sudo groupadd -g 3333 bokkie
sudo useradd -m -s /bin/bash -u 3333 -g 3333 bokkie
sudo mkdir -p /srv/bokkie
sudo chown -R bokkie:bokkie /srv/bokkie

# If you want your normal user to inspect files easily:

sudo usermod -aG bokkie $USERNAME
sudo chmod 2775 /home/bokkie

# I would avoid broader permissions than that.

# 2. Install The Repo As bokkie

#  On both machines:

sudo -u bokkie git clone <your-repo-url> /srv/bokkie
sudo -u bokkie bash -lc 'cd /srv/bokkie && uv sync'

  If the repo already exists under your user, either reclone it cleanly or hand ownership over deliberately. For a service account, I prefer a clean clone.

  3. Set Up Codex Auth For bokkie

  On each machine where Codex will run:

  sudo -u bokkie mkdir -p /home/bokkie/.codex

  Then place:

  - auth.json
  - optional config.toml
  - optional skills/

  under /home/bokkie/.codex/.

  Make permissions tight:

  sudo chown -R bokkie:bokkie /home/bokkie/.codex
  sudo chmod 700 /home/bokkie/.codex
  sudo chmod 600 /home/bokkie/.codex/auth.json

  If you want to keep auth outside the service home, that also works, but putting it under the service user’s home is the simplest setup.

  4. Set Up SSH From App Server To Dev Box

  Generate a dedicated key as bokkie on the app server:

  sudo -u bokkie ssh-keygen -t ed25519 -f /home/bokkie/.ssh/id_ed25519 -N ""

  Copy the public key to the dev box’s bokkie account:

  sudo -u bokkie ssh-copy-id -i /home/bokkie/.ssh/id_ed25519.pub bokkie@DEVBOX_HOST

  Test it:

  sudo -u bokkie ssh bokkie@DEVBOX_HOST 'hostname && whoami && test -d /srv/bokkie && echo ok'

  This should work without prompting.

  I would also add a small SSH config on the app server:

  Host devbox
    HostName YOUR_DEVBOX_HOSTNAME_OR_IP
    User bokkie
    IdentityFile /home/bokkie/.ssh/id_ed25519
    IdentitiesOnly yes

  Path: /home/bokkie/.ssh/config

  Permissions:

  sudo chown bokkie:bokkie /home/bokkie/.ssh/config
  sudo chmod 600 /home/bokkie/.ssh/config

  5. Environment File For bokkie

  Create /srv/bokkie/.env on both hosts.

  App server version should include at least:

  BOKKIE_API_HOST=0.0.0.0
  BOKKIE_API_PORT=8008
  BOKKIE_API_BASE_URL=http://APP_SERVER_IP_OR_DNS:8008

  BOKKIE_DATABASE_URL=postgresql+psycopg://postgres:postgres@localhost:5432/bokkie

  BOKKIE_REPO_ROOT=/srv/bokkie
  BOKKIE_BOKKIE_CONFIG_PATH=/srv/bokkie/bokkie.toml
  BOKKIE_RUNS_ROOT=/srv/bokkie/.bokkie/runs
  BOKKIE_ARTIFACTS_DIR=/srv/bokkie/.bokkie/runs
  BOKKIE_WORKER_CACHE_DIR=/srv/bokkie/.worker-cache
  BOKKIE_WORKER_WORKTREE_DIR=/srv/bokkie/.worker-worktrees

  BOKKIE_DISPATCHER_ENABLED=true
  BOKKIE_DISPATCHER_POLL_SECONDS=10
  BOKKIE_EXECUTOR_LAUNCH_COOLDOWN_SECONDS=30

  Dev box version should include at least:

  BOKKIE_API_BASE_URL=http://APP_SERVER_IP_OR_DNS:8008

  BOKKIE_REPO_ROOT=/srv/bokkie
  BOKKIE_BOKKIE_CONFIG_PATH=/srv/bokkie/bokkie.toml
  BOKKIE_RUNS_ROOT=/srv/bokkie/.bokkie/runs
  BOKKIE_ARTIFACTS_DIR=/srv/bokkie/.bokkie/runs
  BOKKIE_WORKER_CACHE_DIR=/srv/bokkie/.worker-cache
  BOKKIE_WORKER_WORKTREE_DIR=/srv/bokkie/.worker-worktrees

  Because the worker talks to the API only, it does not need DB access.

  6. bokkie.toml Executor Config

  I would set the devbox executor in your current bokkie.toml like this:

  [executors.local]
  driver = "local"
  pools = ["cpu-small", "cpu-large"]
  labels = ["cpu", "small", "internet"]
  workdir = "/srv/bokkie"
  max_workers = 1

  [executors.devbox]
  driver = "ssh-docker"
  host = "devbox"
  pools = ["cpu-large", "gpu-3090"]
  labels = ["cpu", "highmem", "gpu:rtx3090", "internet"]
  workdir = "/srv/bokkie"
  max_workers = 2
  worker_command = "cd /srv/bokkie && export BOKKIE_API_BASE_URL=http://
  APP_SERVER_IP_OR_DNS:8008 && uv run bokkie worker --once --worker-id {worker_id} --host
  {host} --executor-name {executor_name} --target-phase-attempt-id {target_phase_attempt_id}
  --pool cpu-large --pool gpu-3090 --label cpu --label highmem --label gpu:rtx3090 --label
  internet"

  That is the simplest first version. Despite the executor name ssh-docker, the implementation
  currently runs an SSH command, and worker_command is the right place to decide whether that
  command is direct uv or docker run.

  If you want Docker later, replace only worker_command.

  7. Systemd Units

  I would run the API on the app server as a service, and let the dispatcher live inside that
  process for now.

  App server unit:

  /etc/systemd/system/bokkie-api.service

  [Unit]
  Description=Bokkie API
  After=network.target postgresql.service
  Wants=network.target

  [Service]
  User=bokkie
  Group=bokkie
  WorkingDirectory=/srv/bokkie
  EnvironmentFile=/srv/bokkie/.env
  ExecStart=/home/bokkie/.local/bin/uv run bokkie api --host 0.0.0.0 --port 8008
  Restart=always
  RestartSec=5

  [Install]
  WantedBy=multi-user.target

  If uv is not in /home/bokkie/.local/bin/uv, use which uv under the bokkie user and set the
  actual path.

  Enable it:

  sudo systemctl daemon-reload
  sudo systemctl enable --now bokkie-api

  You do not need a permanent worker service on the dev box if you are using dispatcher-
  launched one-shot workers. That is one of the benefits of this slice.

  8. Forgejo / Git Credentials

  For pushes from Bokkie-created branches, I would give bokkie its own machine credential:

  - a dedicated SSH deploy key or machine user key
  - not your personal Git credential

  That way repo writes done by automation are clearly attributable to automation.

  9. First Connectivity Checks

  As bokkie on the app server:

  cd /srv/bokkie
  uv run bokkie api --host 0.0.0.0 --port 8008

  As bokkie on the dev box:

  curl http://APP_SERVER_IP_OR_DNS:8008/api/runs
  cd /srv/bokkie
  uv run bokkie worker --once --worker-id devbox-test --host devbox --executor-name devbox
  --pool cpu-large --label cpu --label highmem --label internet

  As bokkie on the app server:

  ssh devbox 'cd /srv/bokkie && uv run bokkie worker --once --worker-id smoke --host devbox
  --executor-name devbox --pool cpu-large --label cpu --label highmem --label internet'

  Once those work, the dispatcher path should work too.

  10. What I’d Do First In Practice

  The smallest safe rollout is:

  1. Create bokkie on both hosts.
  2. Clone repo to /srv/bokkie on both hosts.
  3. Install uv and sync deps for bokkie.
  4. Place Codex auth in /home/bokkie/.codex/ on the dev box.
  5. Set up SSH from app server bokkie to dev box bokkie.
  6. Set executors.devbox.worker_command to direct uv run bokkie worker --once ....
  7. Start the API service on the app server.
  8. Trigger one manual dispatch from the browser at /ui/executors.
  9. Only after that works, consider swapping the remote worker_command to Docker.

  If you want, the next thing I can do is write the exact bokkie.toml worker_command and two
  systemd units for your real hostnames and paths, if you give me:

  - app server hostname or IP
  - dev box hostname or IP
  - remote repo path if not /srv/bokkie
  - whether you want direct uv or Docker on the dev box

