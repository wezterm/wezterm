# Console Host

This directory contains a copy of built artifacts from the Microsoft
Terminal project which is provided by Microsoft under the terms
of the MIT license.

Why are they here?  At the time of writing, the conpty implementation
that ships with windows is lacking support for mouse reporting but
that support is available in the opensource project so it is desirable
to point to that so that we can enable mouse reporting in wezterm.

It looks like we'll eventually be able to drop this once Windows
and/or the build for the terminal project make some more progress.

https://github.com/wezterm/wezterm/issues/1927

Run `./assets/windows/conhost/update.sh` to refresh these artifacts from
the latest stable Windows Terminal release, or pass a tag
(eg. `v1.24.11321.0`) to pin to a specific release.

To build from source instead, clone <https://github.com/microsoft/terminal>,
run `.\tools\razzle.cmd` followed by `bcz rel`, and copy the artifacts from
`bin/x64/Release`.  You may need the Visual C++ runtime support package
from <https://www.microsoft.com/en-us/download/details.aspx?id=53175>.
