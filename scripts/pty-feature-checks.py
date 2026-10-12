#!/usr/bin/env python3
"""PTY feature checks for an installed Mellow binary.

Each check drives the binary in a real terminal and asserts on files written to
disk or on the rendered screen, for workflows the Rust smoke tests do not cover:
the terminal panel, file tree, palette commands, themes, Git changes, project
search, format on save, and running over SSH.

    python3 -m venv .venv && .venv/bin/pip install pyte
    MELLOW_BIN=~/.cargo/bin/mellow .venv/bin/python scripts/pty-feature-checks.py [check ...]

Checks run in isolated HOME and XDG directories under MELLOW_FEATURE_WORK
(default ~/.cache/mellow-feature-checks). A failing check writes its last screen
to <work>/<check>/screen.txt. Checks that need `rustfmt` or passwordless
`ssh localhost` report SKIP when those are unavailable. The exit status is 1 if
any check failed.
"""
import fcntl
import json
import os
import pty
import select
import shutil
import signal
import struct
import subprocess
import sys
import termios
import time

import pyte

HOME = os.path.expanduser("~")
BIN = os.path.expanduser(os.environ.get("MELLOW_BIN", os.path.join(HOME, ".cargo/bin/mellow")))
WORK = os.path.expanduser(
    os.environ.get("MELLOW_FEATURE_WORK", os.path.join(HOME, ".cache/mellow-feature-checks"))
)
COLS, ROWS = 120, 36


class Skip(Exception):
    """A check whose prerequisite is missing on this host."""

KEYS = {
    "enter": b"\r",
    "esc": b"\x1b",
    "tab": b"\t",
    "backspace": b"\x7f",
    "up": b"\x1b[A",
    "down": b"\x1b[B",
    "right": b"\x1b[C",
    "left": b"\x1b[D",
    "pgup": b"\x1b[5~",
    "pgdn": b"\x1b[6~",
    "f1": b"\x1bOP",
    "f1_vt": b"\x1b[11~",
    "f2": b"\x1bOQ",
    "f6": b"\x1b[17~",
    "alt_down": b"\x1b[1;3B",
    "alt_shift_down": b"\x1b[1;4B",
    "alt_z": b"\x1bz",
    "ctrl_pgdn": b"\x1b[6;5~",
    "ctrl_pgup": b"\x1b[5;5~",
    "ctrl_slash": b"\x1f",
    "ctrl_backslash": b"\x1c",
}
for letter in "abcdefghijklmnopqrstuvwxyz":
    KEYS["ctrl_" + letter] = bytes([ord(letter) & 0x1F])


def spawn(argv, cwd, home, extra_env=None):
    env = {k: v for k, v in os.environ.items() if not k.startswith("MELLOW_")}
    env.update(
        {
            "HOME": home,
            "XDG_CONFIG_HOME": os.path.join(home, "config"),
            "XDG_STATE_HOME": os.path.join(home, "state"),
            "SHELL": "/bin/sh",
            "TERM": "xterm-256color",
            "PATH": os.path.join(HOME, ".cargo/bin") + ":" + os.environ.get("PATH", ""),
            "RUSTUP_HOME": os.path.join(HOME, ".rustup"),
            "CARGO_HOME": os.path.join(HOME, ".cargo"),
        }
    )
    env.update(extra_env or {})
    pid, fd = pty.fork()
    if pid == 0:
        try:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
            os.chdir(cwd)
            os.execvpe(argv[0], argv, env)
        finally:
            os._exit(127)
    return pid, fd


class Session:
    def __init__(self, argv, cwd, home, extra_env=None):
        self.screen = pyte.Screen(COLS, ROWS)
        self.stream = pyte.ByteStream(self.screen)
        self.raw = b""
        self.answered = {b"\x1b[6n": 0, b"\x1b[c": 0}
        self.alive = True
        self.pid, self.fd = spawn(argv, cwd, home, extra_env)

    def pump(self, secs=0.2):
        end = time.time() + secs
        while self.alive and time.time() < end:
            ready, _, _ = select.select([self.fd], [], [], max(0.0, end - time.time()))
            if not ready:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                data = b""
            if not data:
                self.alive = False
                break
            self.raw += data
            self.stream.feed(data)
            self._answer_queries()

    def _answer_queries(self):
        replies = {b"\x1b[6n": b"\x1b[1;1R", b"\x1b[c": b"\x1b[?62c"}
        for query, reply in replies.items():
            while self.answered[query] < self.raw.count(query):
                os.write(self.fd, reply)
                self.answered[query] += 1

    def send(self, *keys, wait=0.3):
        for key in keys:
            os.write(self.fd, KEYS[key])
            self.pump(0.05)
        self.pump(wait)

    def type(self, text, wait=0.3):
        os.write(self.fd, text.encode())
        self.pump(wait)

    def text(self):
        return "\n".join(line.rstrip() for line in self.screen.display)

    def wait_for(self, predicate, timeout=8.0):
        end = time.time() + timeout
        while time.time() < end:
            self.pump(0.15)
            if predicate(self.text()):
                return True
        return predicate(self.text())

    def kill(self):
        try:
            os.kill(self.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            os.waitpid(self.pid, 0)
        except ChildProcessError:
            pass
        self.alive = False


def must(session, predicate, what, timeout=8.0):
    if not session.wait_for(predicate, timeout):
        raise AssertionError(f"{what} not on screen")


class Ctx:
    def __init__(self, name, ws, home):
        self.name, self.ws, self.home = name, ws, home
        self.sessions = []

    def write(self, rel, text):
        path = os.path.join(self.ws, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(text)

    def read(self, rel):
        with open(os.path.join(self.ws, rel)) as f:
            return f.read()

    def config(self, rel, text):
        path = os.path.join(self.home, "config", "mellow", rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(text)

    def git(self, *args):
        subprocess.run(
            ["git", "-c", "user.name=ft", "-c", "user.email=ft@example.com", *args],
            cwd=self.ws,
            check=True,
            capture_output=True,
            text=True,
        )

    def start(self, args=(), extra_env=None):
        s = Session([BIN, *args, "--no-mouse"], self.ws, self.home, extra_env)
        self.sessions.append(s)
        end = time.time() + 6
        while time.time() < end and b"\x1b[?1049h" not in s.raw:
            s.pump(0.1)
        if b"\x1b[?1049h" not in s.raw:
            raise AssertionError("mellow did not start; screen: " + s.text())
        s.pump(0.5)
        return s

    def expect_file(self, s, rel, expected, timeout=8.0):
        end = time.time() + timeout
        actual = None
        while time.time() < end:
            s.pump(0.2)
            try:
                actual = self.read(rel)
            except FileNotFoundError:
                actual = None
            if actual == expected:
                return
        raise AssertionError(f"{rel}: expected {expected!r}, got {actual!r}")


# ---- scenarios -------------------------------------------------------------


def s_terminal_panel(c):
    c.write("a.txt", "hello\n")
    s = c.start(["a.txt"])
    s.send("ctrl_t", wait=1.0)
    s.type("touch from_terminal.txt\r", wait=0.5)
    end = time.time() + 8
    while time.time() < end and not os.path.exists(os.path.join(c.ws, "from_terminal.txt")):
        s.pump(0.2)
    if not os.path.exists(os.path.join(c.ws, "from_terminal.txt")):
        raise AssertionError("shell command typed in terminal panel did not run")
    s.send("ctrl_t", wait=0.6)
    must(s, lambda t: "hello" in t, "editor text after Ctrl+T returns")


def s_file_tree(c):
    c.write("a.txt", "hello\n")
    c.write("tree_probe.txt", "x\n")
    s = c.start(["a.txt"])
    s.send("ctrl_b", wait=0.8)
    must(s, lambda t: "tree_probe.txt" in t, "file tree entry after Ctrl+B")
    s.send("ctrl_b", wait=0.6)
    if "tree_probe.txt" in s.text():
        raise AssertionError("file tree still visible after second Ctrl+B")


def s_goto_line(c):
    c.write("g.txt", "one\ntwo\nthree\n")
    s = c.start(["g.txt"])
    s.send("ctrl_g", wait=0.5)
    s.type("3\r", wait=0.5)
    s.type("X", wait=0.2)
    s.send("ctrl_s", wait=0.4)
    c.expect_file(s, "g.txt", "one\ntwo\nXthree\n")


def s_comment_line(c):
    c.write("main.rs", "fn main() {}\n")
    s = c.start(["main.rs"])
    s.send("ctrl_slash", wait=0.5)
    s.send("ctrl_s", wait=0.4)
    c.expect_file(s, "main.rs", "// fn main() {}\n")


def s_move_line(c):
    c.write("m.txt", "1\n2\n3\n")
    s = c.start(["m.txt"])
    s.send("alt_down", wait=0.5)
    s.send("ctrl_s", wait=0.4)
    c.expect_file(s, "m.txt", "2\n1\n3\n")


def s_duplicate_line(c):
    c.write("d.txt", "1\n2\n")
    s = c.start(["d.txt"])
    s.send("alt_shift_down", wait=0.5)
    s.send("ctrl_s", wait=0.4)
    c.expect_file(s, "d.txt", "1\n1\n2\n")


def s_undo_redo(c):
    c.write("u.txt", "")
    s = c.start(["u.txt"])
    s.type("abc", wait=0.5)
    s.send("ctrl_z", wait=0.4)
    s.send("ctrl_s", wait=0.4)
    c.expect_file(s, "u.txt", "")
    s.send("ctrl_y", wait=0.4)
    s.send("ctrl_s", wait=0.4)
    c.expect_file(s, "u.txt", "abc")


def palette(s, command, wait=0.5):
    s.send("ctrl_p", wait=0.4)
    s.type(command, wait=wait)
    s.send("enter", wait=0.6)


def s_format_on_save(c):
    probe = subprocess.run(["rustfmt", "--version"], capture_output=True, text=True)
    if probe.returncode != 0:
        raise Skip("rustfmt is not runnable (install it or set a default rustup toolchain)")
    c.config("settings.conf", "format_on_save = true\n")
    c.write("main.rs", 'fn main(){println!("hi");}\n')
    s = c.start(["main.rs"])
    palette(s, "Trust workspace execution")
    s.send("enter", wait=0.6)
    s.send("ctrl_s", wait=0.5)
    c.expect_file(s, "main.rs", 'fn main() {\n    println!("hi");\n}\n', timeout=12)


def s_custom_keymap(c):
    c.config("keybindings.conf", "file.save = F2\n")
    c.write("k.txt", "")
    s = c.start(["k.txt"])
    s.type("kx", wait=0.4)
    s.send("f2", wait=0.5)
    c.expect_file(s, "k.txt", "kx")


def _colours(s):
    return {s.screen.buffer[y][x].bg for y in range(ROWS) for x in range(COLS)}


def s_theme_changes_colours(c):
    c.write("t.txt", "hello\n")
    seen = {}
    for theme in ["dark", "gruvbox-dark"]:
        c.config("settings.conf", f"theme = {theme}\n")
        s = c.start(["t.txt"])
        must(s, lambda t: "hello" in t, f"text under theme {theme}")
        seen[theme] = _colours(s)
        s.kill()
        c.sessions.remove(s)
    if seen["dark"] == seen["gruvbox-dark"]:
        raise AssertionError("dark and gruvbox-dark render identical colours")


def s_tabs_switch_and_save(c):
    c.write("a.txt", "A\n")
    c.write("b.txt", "B\n")
    s = c.start(["a.txt", "b.txt"])
    # Mellow opens the last file given on the command line (b.txt) as active.
    palette(s, "Next tab")
    s.type("Q", wait=0.3)
    s.send("ctrl_s", wait=0.5)
    c.expect_file(s, "a.txt", "QA\n")
    c.expect_file(s, "b.txt", "B\n")


def s_open_file_palette(c):
    c.write("a.txt", "A\n")
    c.write("bee.txt", "B\n")
    s = c.start(["a.txt"])
    s.send("ctrl_o", wait=0.5)
    s.type("bee", wait=0.6)
    s.send("enter", wait=0.8)
    s.type("Q", wait=0.3)
    s.send("ctrl_s", wait=0.5)
    c.expect_file(s, "bee.txt", "QB\n")


def s_settings_screen(c):
    c.write("a.txt", "A\n")
    s = c.start(["a.txt"])
    s.send("ctrl_p", wait=0.4)
    s.type("Open settings", wait=0.5)
    s.send("enter", wait=0.6)
    must(s, lambda t: "setting" in t.lower(), "settings screen")


def s_help_screen(c):
    c.write("a.txt", "A\n")
    s = c.start(["a.txt"])
    # Help is bound to F1. Try the xterm SS3 encoding, then the VT encoding.
    s.send("f1", wait=0.8)
    if "Ctrl+" not in s.text():
        s.send("f1_vt", wait=0.8)
    must(s, lambda t: "Ctrl+" in t, "keys listed by F1 help")
    s.send("esc", wait=0.4)


def s_word_wrap(c):
    c.write("w.txt", "W" * 200 + "\n")
    s = c.start(["w.txt"])
    before = s.text()
    s.send("alt_z", wait=0.6)
    after = s.text()
    if "Word wrap: on" not in after or "↪" not in after:
        raise AssertionError("Alt+Z did not turn word wrap on with a continuation row")
    if "↪" in before:
        raise AssertionError("word wrap continuation row was already shown before Alt+Z")


def s_split_pane(c):
    c.write("a.txt", "A\n")
    c.write("b.txt", "B\n")
    s = c.start(["a.txt", "b.txt"])
    s.send("ctrl_backslash", wait=0.8)
    must(s, lambda t: "a.txt" in t and "b.txt" in t, "both files visible after split")


def s_project_search(c):
    c.write("x.txt", "needle_xyz here\n")
    c.write("y.txt", "nothing\n")
    s = c.start(["x.txt"])
    s.send("ctrl_f", wait=0.2)
    s.send("ctrl_f", wait=0.6)
    s.type("needle_xyz\r", wait=1.5)
    must(s, lambda t: "x.txt" in t and "needle_xyz" in t, "project search hit for x.txt")


def s_git_changes_panel(c):
    c.write("a.txt", "one\n")
    c.git("init", "-q")
    c.git("add", "a.txt")
    c.git("commit", "-q", "-m", "init")
    c.write("a.txt", "two\n")
    s = c.start(["a.txt"])
    s.send("ctrl_p", wait=0.4)
    s.type("Show Git changes", wait=0.5)
    s.send("enter", wait=0.6)
    must(s, lambda t: "a.txt" in t, "a.txt in Git changes list")


SSH_OPTIONS = ["-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=accept-new"]


def start_over_ssh(c, name):
    """Opens NAME in Mellow through `ssh -tt localhost`, as a remote user would."""
    reachable = subprocess.run(
        ["ssh", *SSH_OPTIONS, "localhost", "true"], capture_output=True, timeout=20
    )
    if reachable.returncode != 0:
        raise Skip("passwordless `ssh localhost` is not set up")
    remote = (
        f"cd '{c.ws}' && env HOME='{c.home}' XDG_CONFIG_HOME='{c.home}/config' "
        f"XDG_STATE_HOME='{c.home}/state' TERM=xterm-256color {BIN} {name} --no-mouse"
    )
    s = Session(["ssh", "-tt", *SSH_OPTIONS, "localhost", remote], c.ws, c.home, extra_env={"HOME": HOME})
    c.sessions.append(s)
    return s


def s_ssh_session(c):
    c.write("ssh.txt", "over ssh\n")
    s = start_over_ssh(c, "ssh.txt")
    must(s, lambda t: "over ssh" in t, "file text through ssh", timeout=15)
    s.type("X", wait=0.3)
    s.send("ctrl_s", wait=0.4)
    c.expect_file(s, "ssh.txt", "Xover ssh\n")


def tree_has(session, name):
    """True if NAME is listed in the FILES column, not the editor or terminal panel."""
    tree_width = 28
    # Terminal panel rows are drawn inside a box that starts with a border glyph.
    return any(
        name in line[:tree_width]
        for line in session.screen.display
        if not line.startswith(("\u2502", "\u256d", "\u2570"))
    )


def s_tree_refresh_after_terminal(c):
    c.write("a.txt", "hello\n")
    s = c.start(["a.txt"])
    s.send("ctrl_b", wait=0.8)
    s.send("ctrl_t", wait=1.0)
    s.type("touch new_from_term.txt\r", wait=0.5)
    end = time.time() + 8
    while time.time() < end and not os.path.exists(os.path.join(c.ws, "new_from_term.txt")):
        s.pump(0.2)
    s.send("ctrl_t", wait=1.0)
    end = time.time() + 4
    while time.time() < end and not tree_has(s, "new_from_term.txt"):
        s.pump(0.2)
    if not tree_has(s, "new_from_term.txt"):
        raise AssertionError("file created in terminal is not listed in the FILES tree after Ctrl+T back")


def s_tree_refresh_while_terminal_open(c):
    c.write("a.txt", "hello\n")
    s = c.start(["a.txt"])
    s.send("ctrl_b", wait=0.8)
    s.send("ctrl_t", wait=1.0)
    s.type("touch seen_in_panel.txt\r", wait=0.5)
    end = time.time() + 8
    while time.time() < end and not os.path.exists(os.path.join(c.ws, "seen_in_panel.txt")):
        s.pump(0.2)
    s.pump(2.0)
    if not tree_has(s, "seen_in_panel.txt"):
        raise AssertionError("file created in terminal is not listed in the FILES tree while terminal is open")


def s_shebang_script_language(c):
    c.write("deploy", "#!/bin/bash\nif true; then echo hi; fi\n")
    s = c.start(["deploy"])
    must(s, lambda t: "Shell" in t.split("\n")[-1], "status bar shows Shell for extensionless script with bash shebang")


def s_sh_extension_language(c):
    c.write("deploy.sh", "#!/bin/bash\nif true; then echo hi; fi\n")
    s = c.start(["deploy.sh"])
    must(s, lambda t: "Shell" in t.split("\n")[-1], "status bar shows Shell for .sh file")


def s_ssh_disconnect_exits(c):
    c.write("dc.txt", "over ssh\n")
    s = start_over_ssh(c, "dc.txt")
    must(s, lambda t: "over ssh" in t, "file text through ssh", timeout=15)
    s.kill()  # the SSH client dies, as on a dropped connection
    end = time.time() + 8
    while time.time() < end:
        found = subprocess.run(["pgrep", "-f", "^" + BIN + " dc.txt"], capture_output=True, text=True).stdout.strip()
        if not found:
            return
        time.sleep(0.5)
    raise AssertionError(f"mellow still running after SSH disconnect (pids {found})")


SCENARIOS = [
    ("terminal_panel", s_terminal_panel),
    ("file_tree", s_file_tree),
    ("goto_line", s_goto_line),
    ("comment_line", s_comment_line),
    ("move_line", s_move_line),
    ("duplicate_line", s_duplicate_line),
    ("undo_redo", s_undo_redo),
    ("format_on_save_rustfmt", s_format_on_save),
    ("custom_keymap", s_custom_keymap),
    ("theme_changes_colours", s_theme_changes_colours),
    ("tabs_switch_and_save", s_tabs_switch_and_save),
    ("open_file_palette", s_open_file_palette),
    ("settings_screen", s_settings_screen),
    ("help_screen", s_help_screen),
    ("word_wrap", s_word_wrap),
    ("split_pane", s_split_pane),
    ("project_search", s_project_search),
    ("git_changes_panel", s_git_changes_panel),
    ("ssh_session", s_ssh_session),
    ("tree_refresh_after_terminal", s_tree_refresh_after_terminal),
    ("tree_refresh_while_terminal_open", s_tree_refresh_while_terminal_open),
    ("shebang_script_language", s_shebang_script_language),
    ("sh_extension_language", s_sh_extension_language),
    ("ssh_disconnect_exits", s_ssh_disconnect_exits),
]


def run_scenario(name, fn):
    base = os.path.join(WORK, name)
    shutil.rmtree(base, ignore_errors=True)
    ws = os.path.join(base, "ws")
    home = os.path.join(base, "home")
    os.makedirs(ws)
    os.makedirs(home)
    ctx = Ctx(name, ws, home)
    try:
        fn(ctx)
        return "PASS", ""
    except Skip as reason:
        return "SKIP", str(reason)
    except Exception as error:  # report every failure, keep going
        screen_path = os.path.join(base, "screen.txt")
        if ctx.sessions:
            with open(screen_path, "w") as f:
                f.write(ctx.sessions[-1].text())
        return "FAIL", f"{type(error).__name__}: {error} (screen: {screen_path})"
    finally:
        for s in ctx.sessions:
            s.kill()


def main():
    only = set(sys.argv[1:])
    unknown = only - {name for name, _ in SCENARIOS}
    if unknown:
        sys.exit(f"unknown check(s): {', '.join(sorted(unknown))}")
    if not os.access(BIN, os.X_OK):
        sys.exit(f"no Mellow binary at {BIN}; set MELLOW_BIN")
    os.makedirs(WORK, exist_ok=True)
    results = []
    for name, fn in SCENARIOS:
        if only and name not in only:
            continue
        status, message = run_scenario(name, fn)
        print(f"{status} {name}" + (f" :: {message}" if message else ""), flush=True)
        results.append({"name": name, "status": status, "message": message})
    with open(os.path.join(WORK, "results.json"), "w") as f:
        json.dump(results, f, indent=1)
    count = {status: sum(r["status"] == status for r in results) for status in ("PASS", "FAIL", "SKIP")}
    print(f"SUMMARY {count['PASS']} passed, {count['FAIL']} failed, {count['SKIP']} skipped", flush=True)
    sys.exit(1 if count["FAIL"] else 0)


if __name__ == "__main__":
    main()
