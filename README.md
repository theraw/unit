# NGINX Unit

[![Project Status: Active.](https://www.repostatus.org/badges/latest/active.svg)](https://www.repostatus.org/#active)

> Downstream fork maintained at [theraw/unit](https://github.com/theraw/unit) by
> Julio S. (raweb.al). Upstream `nginx/unit` is archived; security and
> behavior patches land here and ship as prebuilt packages on
> [apt.julio.al](https://apt.julio.al).

## Universal Web App Server

![NGINX Unit Logo](docs/unitlogo.svg)

NGINX Unit is a lightweight and versatile open-source server that has
two primary capabilities:

- serves static media assets,
- runs application code in eight languages.

Unit compresses several layers of the modern application stack into a potent,
coherent solution with a focus on performance, low latency, and scalability. It
is intended as a universal building block for any web architecture, regardless
of its complexity, from enterprise-scale deployments to your pet's homepage.

Its native [RESTful JSON API](#openapi-specification) enables dynamic
updates with zero interruptions and flexible configuration, while its
out-of-the-box productivity reliably scales to production-grade workloads. We
achieve that with a complex, asynchronous, multithreading architecture
comprising multiple processes to ensure security and robustness while getting
the most out of today's computing platforms.

## Installation — RAWeb prebuilt packages (apt.julio.al)

Per-codename builds are published to a Nexus-hosted apt/yum repo, signed with
the RAWeb GPG key. Every push is validated end-to-end (clean container, install
from the repo, smoke-test `unitd` + control API) before the artifacts go live.

| OS                | Codename | Repo                                                  |
| ----------------- | -------- | ----------------------------------------------------- |
| Debian 13         | trixie   | `https://apt.julio.al/repository/raweb-trixie/`       |
| Ubuntu 24.04 LTS  | noble    | `https://apt.julio.al/repository/raweb-noble/`        |
| Ubuntu 22.04 LTS  | jammy    | `https://apt.julio.al/repository/raweb-jammy/`        |
| AlmaLinux 8       | el8      | `https://apt.julio.al/repository/raweb-alma8/`        |
| AlmaLinux 9       | el9      | `https://apt.julio.al/repository/raweb-alma9/`        |
| AlmaLinux 10      | el10     | `https://apt.julio.al/repository/raweb-alma10/`       |

### Debian / Ubuntu

Substitute `<codename>` for one of `trixie`, `noble`, `jammy`.

```console
$ sudo install -d /etc/apt/keyrings
$ curl -fsSL https://apt.julio.al/repository/public/keys/raweb.asc \
    | sudo gpg --dearmor -o /etc/apt/keyrings/raweb.gpg
$ echo "deb [signed-by=/etc/apt/keyrings/raweb.gpg] \
    https://apt.julio.al/repository/raweb-<codename>/ <codename> main" \
    | sudo tee /etc/apt/sources.list.d/raweb-unit.list
$ sudo apt update
$ sudo apt install unit
```

### AlmaLinux / RHEL family

Substitute `<N>` for one of `8`, `9`, `10`.

```console
$ sudo tee /etc/yum.repos.d/raweb-unit.repo >/dev/null <<EOF
[raweb-unit]
name=RAWeb Unit
baseurl=https://apt.julio.al/repository/raweb-alma<N>/
enabled=1
gpgcheck=1
gpgkey=https://apt.julio.al/repository/public/keys/raweb.asc
EOF
$ sudo dnf install -y unit
```

After install, `systemctl enable --now unit` then check it's up:

```console
$ sudo curl --unix-socket /var/run/control.unit.sock http://localhost/config/
{
    "listeners": {},
    "routes": [],
    "applications": {}
}
```

### Building your own packages

The publisher script lives at the repository's parent — see
[`/srv/unit-build.sh`](../unit-build.sh). It spins up a clean incus container
per codename, builds inside it (so unitd is linked against that distro's
libssl/libpcre2 SONAME), signs with the RAWeb GPG key, and uploads to the
matching Nexus repo. `bash /srv/unit-build.sh help` shows the full target
list.

## Other installation methods

### macOS

```console
$ brew install nginx/unit/unit
```

For details and available language packages, see the
[docs](https://unit.nginx.org/installation/#homebrew).

### Docker

```console
$ docker pull unit:<TAG>
$ mkdir /tmp/unit-control # customize as needed.
$ docker run -d \
      --mount type=bind,src=/tmp/unit-control,dst=/var/run \
      --mount type=bind,src=.,dst=/www \
      --network host \
      unit
```

For a description of image tags, see the
[docs](https://unit.nginx.org/installation/#docker-images).

Your current working directory will now be mounted to the Unit image at `/www`.
You can reach its socket at `/tmp/unit-control/control.unit.sock` assuming no
further customizations have been made.

## Getting Started with `unitctl`

[`unitctl`](tools/README.md) streamlines the management of NGINX Unit processes
through an easy-to-use command line interface. To get started with `unitctl`,
download it from the
[official GitHub releases](https://github.com/nginx/unit/releases)
or [Homebrew](#macos).

### Installation

> [!NOTE]
> If you installed Unit with [Homebrew](#macos), you can skip this step
> as `unitctl` is included by default.

Download the appropriate `unitctl` binary for your system from the
[NGINX Unit releases](https://github.com/nginx/unit/releases/).

``` console
$ tar xzvf unitctl-master-x86_64-unknown-linux-gnu.tar.gz
# mv unitctl /usr/local/bin/
```


## Launch Unit using Docker
If you have [Docker installed](https://docs.docker.com/engine/install/) on
your machine, and then you can effortlessly spin up one of
[Unit's official Docker images](https://hub.docker.com/_/unit)
alongside your application.

> [!TIP]
> How-to and configuration guides are available on
[unit.nginx.org](https://unit.nginx.org/howto/) for web application frameworks
built with Python, PHP, WebAssembly, Node.js, Ruby, and more.

Here's an example using the `unit:python` Docker image:
``` console
$ unitctl instances new 127.0.0.1:8001 /path/to/app 'unit:python'
```

`/path/to/app` will mount to `/www` in the Docker filesystem.

Save this to `/path/to/app/wsgi.py`:
```python
def application(environ, start_response):
    start_response("200 OK", [("Content-Type", "text/plain")])
    return (b"Hello, Python on Unit!")
```

You can then interactively edit the currently active configuration:
``` console
$ unitctl edit
```
```jsonc
{
  "listeners": {
    "*:8000": {
      // Point listener to new application
      "pass": "applications/python"
    }
  },

  // Add an application definition
  "applications": {
    "python": {
        "type": "python",
        "path": "/www/",
        "module": "wsgi"
    }
  }
}
```
Valid configurations will be applied upon save and close.

``` console
$ curl localhost:8000

Hello, Python on Unit!
```
More Python configuration examples can be found in the
[Unit docs](https://unit.nginx.org/howto/samples/#python).

## Hello World with PHP and curl

Unit runs apps in a
[variety of languages](https://unit.nginx.org/howto/samples/).
Let's explore the configuration of a simple PHP app on Unit with `curl`.

Suppose you saved a PHP script as `/www/helloworld/index.php`:
``` php
<?php echo "Hello, PHP on Unit!"; ?>
```

To run it on Unit with the `unit-php` module installed, first set up an
application object. Let's store our first config snippet in a file called
`config.json`:

``` json
{
    "helloworld": {
        "type": "php",
        "root": "/www/helloworld/"
    }
}
```

Saving it as a file isn't necessary, but can come in handy with larger objects.

Now, `PUT` it into the `/config/applications` section of Unit's control API,
usually available by default via a Unix domain socket:

``` console
# curl -X PUT --data-binary @config.json --unix-socket  \
       /path/to/control.unit.sock http://localhost/config/applications
```
``` json
{
	"success": "Reconfiguration done."
}
```

Next, reference the app from a listener object in the `/config/listeners`
section of the API.  This time, we pass the config snippet straight from the
command line:

``` console
# curl -X PUT -d '{"127.0.0.1:8080": {"pass": "applications/helloworld"}}'  \
       --unix-socket /path/to/control.unit.sock http://localhost/config/listeners
```
``` json
{
    "success": "Reconfiguration done."
}
```

Now Unit accepts requests at the specified IP and port, passing them to the
application process. Your app works!

``` console
$ curl 127.0.0.1:8080

      Hello, PHP on Unit!
```

Finally, query the entire `/config` section of the control API:

``` console
# curl --unix-socket /path/to/control.unit.sock http://localhost/config/
```

Unit's output should contain both snippets, neatly organized:

``` json
{
    "listeners": {
        "127.0.0.1:8080": {
            "pass": "applications/helloworld"
        }
    },

    "applications": {
        "helloworld": {
            "type": "php",
            "root": "/www/helloworld/"
        }
    }
}
```

## WebAssembly
Unit supports running WebAssembly Components (WASI 0.2).
For more information see the
[Unit Configuration Docs](https://unit.nginx.org/configuration/#configuration-wasm).

## OpenAPI Specification

Our [OpenAPI specification](docs/unit-openapi.yaml) aims to simplify
configuring and integrating NGINX Unit deployments and provide an authoritative
source of knowledge about the control API.

## Community

- The go-to place to start asking questions and share your thoughts is
 [GitHub Discussions](https://github.com/nginx/unit/discussions).

- Our [GitHub issues page](https://github.com/nginx/unit/issues) offers
  space for a more technical discussion at your own pace.

- The [project map](https://github.com/orgs/nginx/projects/1) on
  GitHub sheds some light on our current work and plans for the future.

- Our [official website](https://unit.nginx.org/) may provide answers
  not easily found otherwise.

- Get involved with the project by contributing! See the
  [contributing guide](CONTRIBUTING.md) for details.

- To reach the team directly, subscribe to the
  [mailing list](https://mailman.nginx.org/mailman/listinfo/unit).

- For security issues, [email us](mailto:security-alert@nginx.org),
  mentioning NGINX Unit in the subject and following the [CVSS
  v3.1](https://www.first.org/cvss/v3.1/specification-document) spec.
