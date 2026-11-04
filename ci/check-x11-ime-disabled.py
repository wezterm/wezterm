#!/usr/bin/env python3
"""Check disabled-IME cursor updates on a disposable X server.

Requires Xvfb, libX11 and a built wezterm-gui. Example:
  python3 ci/check-x11-ime-disabled.py --xvfb /path/to/Xvfb target/debug/wezterm-gui

A deliberately unresponsive XIM selection owner makes unexpected initialization
retries observable even when xcb-imdkit destroys windows between retries. Nothing
connects to the caller's display, input method, mux or configuration.
"""

import argparse
import ctypes as C
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import tempfile
import time


class SelectionRequest(C.Structure):
    _fields_ = [
        ("type", C.c_int), ("serial", C.c_ulong), ("send_event", C.c_int),
        ("display", C.c_void_p), ("owner", C.c_ulong),
        ("requestor", C.c_ulong), ("selection", C.c_ulong),
        ("target", C.c_ulong), ("property", C.c_ulong), ("time", C.c_ulong),
    ]


class MapEvent(C.Structure):
    _fields_ = [
        ("type", C.c_int), ("serial", C.c_ulong), ("send_event", C.c_int),
        ("display", C.c_void_p), ("event", C.c_ulong), ("window", C.c_ulong),
        ("override_redirect", C.c_int),
    ]


class Event(C.Union):
    _fields_ = [("type", C.c_int), ("selection", SelectionRequest),
                ("map", MapEvent), ("padding", C.c_long * 24)]


class ClassHint(C.Structure):
    _fields_ = [("name", C.c_void_p), ("cls", C.c_void_p)]


def xlib():
    x = C.CDLL("libX11.so.6")
    signatures = {
        "XOpenDisplay": (C.c_void_p, [C.c_char_p]),
        "XDefaultRootWindow": (C.c_ulong, [C.c_void_p]),
        "XCreateSimpleWindow": (C.c_ulong, [C.c_void_p, C.c_ulong, C.c_int,
            C.c_int, C.c_uint, C.c_uint, C.c_uint, C.c_ulong, C.c_ulong]),
        "XInternAtom": (C.c_ulong, [C.c_void_p, C.c_char_p, C.c_int]),
        "XSetSelectionOwner": (C.c_int, [C.c_void_p, C.c_ulong, C.c_ulong, C.c_ulong]),
        "XChangeProperty": (C.c_int, [C.c_void_p, C.c_ulong, C.c_ulong,
            C.c_ulong, C.c_int, C.c_int, C.c_void_p, C.c_int]),
        "XSelectInput": (C.c_int, [C.c_void_p, C.c_ulong, C.c_long]),
        "XPending": (C.c_int, [C.c_void_p]),
        "XNextEvent": (C.c_int, [C.c_void_p, C.POINTER(Event)]),
        "XGetClassHint": (C.c_int, [C.c_void_p, C.c_ulong, C.POINTER(ClassHint)]),
        "XSetInputFocus": (C.c_int, [C.c_void_p, C.c_ulong, C.c_int, C.c_ulong]),
        "XSync": (C.c_int, [C.c_void_p, C.c_int]),
        "XFree": (C.c_int, [C.c_void_p]),
        "XCloseDisplay": (C.c_int, [C.c_void_p]),
    }
    for name, (result, args) in signatures.items():
        getattr(x, name).restype = result
        getattr(x, name).argtypes = args
    return x


def stop(process):
    if process is not None and process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--xvfb", default="Xvfb")
    parser.add_argument("--font", help="Installed font family for builds without vendored fonts")
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    app = server = None
    display = None
    x = xlib()
    with tempfile.TemporaryDirectory(prefix="wezterm-ime-check-") as directory:
        directory = Path(directory)
        log_path = directory / "process.log"
        with log_path.open("w+") as log:
            try:
                read_fd, write_fd = os.pipe()
                try:
                    server = subprocess.Popen(
                        [args.xvfb, "-displayfd", str(write_fd), "-screen", "0",
                         "800x600x24", "-nolisten", "tcp", "-ac"],
                        pass_fds=(write_fd,), stdout=log, stderr=log,
                        start_new_session=True)
                finally:
                    os.close(write_fd)
                try:
                    if not select.select([read_fd], [], [], 10)[0]:
                        raise RuntimeError("Xvfb startup timed out")
                    number = os.read(read_fd, 32).decode().strip()
                finally:
                    os.close(read_fd)
                if not number.isdecimal():
                    raise RuntimeError("Xvfb did not provide a display number")
                display_name = ":" + number
                display = x.XOpenDisplay(display_name.encode())
                if not display:
                    raise RuntimeError("Cannot connect to disposable display")
                root = x.XDefaultRootWindow(display)
                owner = x.XCreateSimpleWindow(display, root, 0, 0, 1, 1, 0, 0, 0)
                selection = x.XInternAtom(display, b"@server=wezterm-ime-check", 0)
                servers = x.XInternAtom(display, b"XIM_SERVERS", 0)
                transport = x.XInternAtom(display, b"TRANSPORT", 0)
                x.XSetSelectionOwner(display, selection, owner, 0)
                atoms = (C.c_ulong * 1)(selection)
                x.XChangeProperty(display, root, servers, 4, 32, 0, atoms, 1)
                x.XSelectInput(display, root, 1 << 19)  # SubstructureNotifyMask
                x.XSync(display, 0)

                config = directory / "wezterm.lua"
                font = ""
                if args.font:
                    font = "font=require('wezterm').font(" + json.dumps(
                        args.font, ensure_ascii=False) + "),"
                config.write_text("return {" + font + "use_ime=false, enable_wayland=false, "
                    "front_end='Software', check_for_updates=false, "
                    "automatically_reload_config=false, enable_tab_bar=false, "
                    "initial_cols=60, initial_rows=15}\n")
                workload = directory / "cursor.py"
                workload.write_text("import sys,time\ntime.sleep(3)\n"
                    "for i in range(750):\n"
                    " sys.stdout.write(f'\\x1b[{i%10+1};{i%40+1}HX');sys.stdout.flush()\n"
                    " time.sleep(0.02)\n"
                    "time.sleep(1)\n"
                    "open(sys.argv[1], 'w').write('750')\n")
                env = dict(os.environ, DISPLAY=display_name, WAYLAND_DISPLAY="",
                    XMODIFIERS="@im=wezterm-ime-check", LIBGL_ALWAYS_SOFTWARE="1",
                    DBUS_SESSION_BUS_ADDRESS="unix:path=" + str(directory / "no-bus"))
                env.pop("WEZTERM_UNIX_SOCKET", None)
                app = subprocess.Popen([str(binary), "--config-file", str(config),
                    "start", "--always-new-process", "--", sys.executable,
                    str(workload), str(directory / "completed")], env=env, stdout=log, stderr=log,
                    start_new_session=True)
                requests = []
                focused = False
                deadline = time.monotonic() + 45
                while app.poll() is None:
                    if time.monotonic() > deadline:
                        raise RuntimeError("Terminal did not exit after bounded workload")
                    while x.XPending(display):
                        event = Event()
                        x.XNextEvent(display, C.byref(event))
                        if event.type == 30 and event.selection.target == transport:
                            requests.append(event.selection.requestor)
                        elif event.type == 19:
                            hint = ClassHint()
                            if x.XGetClassHint(display, event.map.window, C.byref(hint)):
                                try:
                                    if hint.cls and b"wezterm" in C.string_at(hint.cls).lower():
                                        x.XSetInputFocus(display, event.map.window, 1, 0)
                                        x.XSync(display, 0)
                                        focused = True
                                finally:
                                    if hint.name:
                                        x.XFree(hint.name)
                                    if hint.cls:
                                        x.XFree(hint.cls)
                    time.sleep(0.01)
                if app.returncode or not focused:
                    raise RuntimeError(f"Terminal exit={app.returncode}, focused={focused}")
                if not (directory / "completed").exists():
                    raise RuntimeError("Cursor workload did not complete")
                # Current xcb-imdkit opens once in its constructor. A disabled
                # cursor path must not trigger further opens, even after timeout.
                print(f"750 cursor moves: {len(requests)} XIM transport requests, "
                      f"{len(set(requests))} requestor window(s)")
                if len(requests) > 1:
                    raise RuntimeError("Disabled IME retried initialization during cursor motion")
                print("PASS: disabled IME does not retry on cursor updates")
            except Exception:
                log.flush()
                print(log_path.read_text(), file=sys.stderr)
                raise
            finally:
                stop(app)
                if display:
                    x.XCloseDisplay(display)
                stop(server)


if __name__ == "__main__":
    main()
