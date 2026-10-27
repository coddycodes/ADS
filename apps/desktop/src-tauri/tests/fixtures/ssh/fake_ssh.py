#!/usr/bin/env python3
"""CODDY_STUDIO-SSH-FIXTURE fake OpenSSH client.

Used only by apps/desktop/src-tauri/tests/ssh_environment.rs, which starts it
through a generated wrapper that sets the SSH_FIXTURE_* variables below. It
never connects to another machine: the "remote host" is the private directory
$SSH_FIXTURE_ROOT/<alias>/remote-home, and remote commands run locally with that
directory as HOME and a clean, login-like environment.

Remote commands follow OpenSSH semantics: ssh(1) joins the remote argv with
single spaces and sshd(8) runs the joined string with `$SHELL -c` (/bin/sh
here). `-N -L LOCAL:HOST:PORT` (the desktop tunnel) runs a local TCP forwarder
that stays alive until it is killed, like a live `ssh -N` session.

By default the fake execs into the remote shell, so killing the client also
kills the remote command. With SSH_FIXTURE_SSHD_SESSIONS=1 it behaves like
sshd(8) without a PTY instead: the remote command runs in its own session and
the fake only relays stdio, so killing the client closes the channel but
signals nothing, and the remote command runs on until it ends by itself or
writes to the closed channel.

Destination aliases starting with `hang-pairing` make the remote `coddy-studio`
wrapper hang on `pairing issue`, to exercise the desktop's pairing deadline.

Environment:
  SSH_FIXTURE_ROOT              per-run state directory (required)
  SSH_FIXTURE_CODDY_STUDIO           the real `coddy-studio` binary (required)
  SSH_FIXTURE_REMOTE_BIN        directory holding the remote `coddy-studio` wrapper
                                (required)
  SSH_FIXTURE_REMOTE_PORT_START first remote port the launch script scans
                                (keeps the fake remote server away from 3773)
  SSH_FIXTURE_SSHD_SESSIONS     1 for sshd-like remote sessions (see above)
"""

import json
import os
import socket
import subprocess
import sys
import threading
import time


def fail(message):
    sys.stderr.write(message + "\n")
    return 255


SCRIPT_HEADER = "# coddy-studio-ssh:"


def classify(remote, script):
    """Names a remote script by its first line, `# coddy-studio-ssh:<kind>`."""
    if remote[:3] == ["sh", "-s", "--"]:
        first_line = script.split("\n", 1)[0]
        if first_line.startswith(SCRIPT_HEADER):
            return first_line[len(SCRIPT_HEADER):].strip()
        return "script"
    return "other"


def pump(source, sink):
    try:
        while True:
            data = source.recv(65536)
            if not data:
                break
            sink.sendall(data)
    except OSError:
        pass
    finally:
        try:
            sink.shutdown(socket.SHUT_WR)
        except OSError:
            pass


def serve_connection(client, host, port):
    try:
        upstream = socket.create_connection((host, port), timeout=10)
        upstream.settimeout(None)
    except OSError:
        client.close()
        return
    forward = threading.Thread(target=pump, args=(client, upstream), daemon=True)
    backward = threading.Thread(target=pump, args=(upstream, client), daemon=True)
    forward.start()
    backward.start()
    forward.join()
    backward.join()
    client.close()
    upstream.close()


def run_tunnel(state, forward):
    if forward is None:
        return fail("fake ssh: -N without -L is not supported")
    parts = forward.split(":")
    if len(parts) != 3:
        return fail("fake ssh: unsupported -L specification " + forward)
    local_port, host, port = int(parts[0]), parts[1], int(parts[2])
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        listener.bind(("127.0.0.1", local_port))
    except OSError:
        # ExitOnForwardFailure=yes
        return fail(
            "bind [127.0.0.1]:%d: Address already in use\n"
            "Could not request local forwarding." % local_port
        )
    listener.listen(64)
    with open(os.path.join(state, "tunnels.jsonl"), "a") as log:
        log.write(json.dumps({"pid": os.getpid(), "local": local_port, "remote": port}) + "\n")
    while True:
        client, _ = listener.accept()
        threading.Thread(target=serve_connection, args=(client, host, port), daemon=True).start()


def relay(source, sink):
    """Copies a remote output pipe to this client's own stream until EOF."""
    while True:
        data = source.read1(65536)
        if not data:
            break
        try:
            sink.write(data)
            sink.flush()
        except OSError:
            break
    source.close()


def run_sshd_session(state, remote_home, command, environment, script):
    """Runs `command` the way sshd(8) runs a session without a PTY.

    The command gets its own session, so nothing this client does (or
    suffers) signals it. The client writes the script to its stdin, relays its
    stdout and stderr, and exits with its status once both reach EOF.
    """
    remote = subprocess.Popen(
        ["/bin/sh", "-c", command],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=environment,
        cwd=remote_home,
        start_new_session=True,
    )
    with open(os.path.join(state, "remote-sessions.jsonl"), "a") as log:
        log.write(json.dumps({"pid": remote.pid, "command": command}) + "\n")
    relays = [
        threading.Thread(target=relay, args=(remote.stdout, sys.stdout.buffer)),
        threading.Thread(target=relay, args=(remote.stderr, sys.stderr.buffer)),
    ]
    for thread in relays:
        thread.start()
    try:
        remote.stdin.write(script.encode())
        remote.stdin.close()
    except OSError:
        pass
    for thread in relays:
        thread.join()
    status = remote.wait()
    return status if status >= 0 else 255


def seed_port(remote_home, state_key, start):
    # The launch script treats an existing port file as the start of its
    # free-port scan and still verifies that the port is free.
    if not start:
        return
    state_dir = os.path.join(remote_home, ".coddy/studio/ssh-launch", state_key)
    port_file = os.path.join(state_dir, "port")
    if not os.path.exists(port_file):
        os.makedirs(state_dir, exist_ok=True)
        with open(port_file, "w") as handle:
            handle.write(start + "\n")


def main():
    root = os.environ.get("SSH_FIXTURE_ROOT")
    real = os.environ.get("SSH_FIXTURE_CODDY_STUDIO")
    remote_bin = os.environ.get("SSH_FIXTURE_REMOTE_BIN")
    if not root or not real or not remote_bin:
        return fail(
            "fake ssh: SSH_FIXTURE_ROOT, SSH_FIXTURE_CODDY_STUDIO and SSH_FIXTURE_REMOTE_BIN must be set"
        )

    args = sys.argv[1:]
    options = []
    forward = None
    no_command = False
    host = None
    index = 0
    while index < len(args):
        arg = args[index]
        if arg in ("-o", "-p", "-L"):
            if index + 1 >= len(args):
                return fail("fake ssh: %s requires a value" % arg)
            options.append([arg, args[index + 1]])
            if arg == "-L":
                forward = args[index + 1]
            index += 2
            continue
        if arg in ("-n", "-N", "-T"):
            options.append([arg])
            if arg == "-N":
                no_command = True
            index += 1
            continue
        if arg.startswith("-"):
            return fail("fake ssh: unsupported option " + arg)
        host = arg
        index += 1
        break
    remote = args[index:]
    if host is None:
        return fail("fake ssh: missing destination")

    alias = host.split("@", 1)[-1]
    state = os.path.join(root, alias)
    remote_home = os.path.join(state, "remote-home")
    os.makedirs(remote_home, exist_ok=True)

    script = ""
    if not no_command and remote[:3] == ["sh", "-s", "--"]:
        # The desktop writes the remote script to stdin and closes it. Keep a
        # copy so the invocation can be classified, then replay it as stdin.
        script = sys.stdin.read()
    kind = "tunnel" if no_command else classify(remote, script)
    record = {
        "time": time.time(),
        "pid": os.getpid(),
        "kind": kind,
        "host": host,
        "options": options,
        "remote": remote,
    }
    with open(os.path.join(state, "ssh-invocations.jsonl"), "a") as log:
        log.write(json.dumps(record) + "\n")

    if no_command:
        return run_tunnel(state, forward)

    if kind == "launch" and len(remote) >= 4:
        start = os.environ.get("SSH_FIXTURE_REMOTE_PORT_START")
        if start:
            # Spread aliases over the scan window so scenarios never race for a port.
            start = str(int(start) + (sum(alias.encode()) % 40) * 5)
        seed_port(remote_home, remote[3], start)

    sshd_sessions = os.environ.get("SSH_FIXTURE_SSHD_SESSIONS") == "1"
    if script and not sshd_sessions:
        script_path = os.path.join(state, "stdin-%d.sh" % os.getpid())
        with open(script_path, "w") as handle:
            handle.write(script)
        # OpenSSH delivers stdin as a stream. A regular file shares seek state
        # across shell forks; macOS sh can replay a comment suffix after its
        # background launch. Keep the fake's exec ownership and replay a pipe.
        payload = script.encode()
        reader, writer = os.pipe()
        feeder = os.fork()
        if feeder == 0:
            os.close(reader)
            try:
                while payload:
                    payload = payload[os.write(writer, payload):]
            except BrokenPipeError:
                pass
            finally:
                os.close(writer)
                os._exit(0)
        os.close(writer)
        os.dup2(reader, 0)
        os.close(reader)

    user = os.environ.get("USER", "fixture")
    environment = {
        "HOME": remote_home,
        "USER": user,
        "LOGNAME": os.environ.get("LOGNAME", user),
        "SHELL": "/bin/sh",
        "LANG": "C.UTF-8",
        "PATH": remote_bin + ":/usr/local/bin:/usr/bin:/bin",
        "SSH_FIXTURE_STATE": state,
        "SSH_FIXTURE_CODDY_STUDIO": real,
        "SSH_FIXTURE_HANG_PAIRING": "1" if alias.startswith("hang-pairing") else "0",
    }
    # Preserve the explicitly selected dev-guard diagnostic mode for the owned
    # remote CLI; production SSH scripts do not manage this test-only variable.
    if "CODDY_STUDIO_HERMETIC_GUARD" in os.environ:
        environment["CODDY_STUDIO_HERMETIC_GUARD"] = os.environ["CODDY_STUDIO_HERMETIC_GUARD"]
    if not remote:
        return fail("fake ssh: interactive sessions are not supported")
    if sshd_sessions:
        return run_sshd_session(state, remote_home, " ".join(remote), environment, script)
    os.chdir(remote_home)
    os.execvpe("/bin/sh", ["/bin/sh", "-c", " ".join(remote)], environment)
    return 255


if __name__ == "__main__":
    sys.exit(main())
