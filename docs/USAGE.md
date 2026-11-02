# Using Oxidrop

## Adding files

Drop files or folders on the window, or use **File → Add files…** (Ctrl+O) or **File → Add
folder…**. Folders are searched for PNG, APNG, GIF, and WebP files. Files and folders can also
be given on the command line:

```sh
oxidrop path/to/images more.png
```

With **Start when files are added** on, files start as soon as they're added; otherwise press
**Start** (Ctrl+Enter).

## Settings

Changes apply to files that haven't started yet.

- **Compression**
  - **Level**: oxipng's preset, from 0 (fastest) to 6 (smallest, and much slower).
  - **Use Zopfli**: much slower compression that usually saves another 3–8%, with an iteration
    count, and optionally a stop after a number of passes without a gain.
- **Image**
  - **Metadata**: strip safe chunks (the ones that don't change how the image looks), keep
    all, or strip all, including color profiles. Every choice keeps an animation's frames.
  - **Interlacing**: remove it (almost always smaller), keep it as is, or add it (Adam7, only
    when that doesn't make the file bigger).
  - **Optimize transparent pixels**: changes the color of fully transparent pixels, which
    can't be seen, when that compresses better.
- **Output**
  - **Overwrite originals**: an original is only replaced when the result is smaller.
  - **Save to folder**: results go to a folder of your choice. Files from dropped folders keep
    their folder structure, and files with the same name get `name (2).png` instead of
    overwriting each other.
  - **Keep original modified time**.
- **Performance**
  - **Files at a time**: how many files are optimized at once. The CPU cores are shared among
    them: a single large file gets every core, a big batch gets one per file.
  - **Time limit per file**: an optional limit; a file that reaches it keeps the smallest
    result found so far.

## While files run

Each running file shows real progress, worked out from oxipng's own log: the reduction
candidates, filter trials k of N, and, with Zopfli, an estimate weighted by bytes that counts
passes against the iteration count. At levels 5 and 6, where every filter is compressed with
Zopfli at once, that's tracked for each filter.

- **Stop** cancels running files right away, without writing anything, and keeps queued ones
  from starting.
- Right-click a row to start or stop just that file, run a finished file again with the
  current settings, open its folder, or copy its path.
- Click a column header to sort.

## Results and the details panel

The file list shows each file's original and new size, the savings, the time taken, and what
changed, for example `RGB+Alpha 8-bit to Indexed 8-bit (40 colors) · stripped tEXt`.

Select a file to see its details:

- Before and after: size, dimensions, pixel format, interlacing, chunks, and the time each
  stage took. The Before column is filled in as soon as a file is added.
- What oxipng chose, and how much of the saving came from the image data versus other chunks.
- How many CPU threads the file had.
- How the reduction candidates and filter trials compared.
- A chart of Zopfli's passes, with a hint on whether more iterations would help.

## GIF and WebP files

GIF and WebP files, animated or not, are converted to new PNG files next to the original (or
in the output folder), then optimized. The original is never replaced.

- Animations become APNG and play as they do in browsers. Each frame stores only what changed,
  with a palette when all the frames fit in 256 colors.
- ICC profiles, EXIF, and XMP are carried over, then kept or stripped by the Metadata setting.
- Lossy WebP files usually get larger as PNG; the PNG is written anyway.

## Keyboard shortcuts

| Keys                            | Action                                                |
|---------------------------------|-------------------------------------------------------|
| Ctrl+O                          | Add files                                             |
| Ctrl+Enter                      | Start                                                 |
| Delete                          | Remove the selected file from the list                |
| Escape                          | Close an open menu, or else the About window          |
| Alt+F, Alt+V, Alt+H             | Open the File, View, and Help menus (Option on macOS) |
| A menu item's underlined letter | Pick it from the open menu (Alt+F, then Q, quits)     |

On macOS, Cmd takes the place of Ctrl, and Delete is Fn+Delete.

## Settings and crash logs

Settings are saved in:

- Windows: `%APPDATA%\oxidrop\data`
- Linux: `~/.local/share/oxidrop`
- macOS: `~/Library/Application Support/oxidrop`

If Oxidrop crashes, it says so in a dialog and writes the details to `crash.log` in the same
folder. Include it when you report the problem.

## Graphics problems

**Help → About Oxidrop** shows the graphics backend and adapter in use. To try a different
backend, set `WGPU_BACKEND` to `vulkan`, `dx12`, or `gl` before starting Oxidrop.
