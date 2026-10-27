# The Scrolling Layout

In the Scrolling layout every window is a column in a row that is as long as it needs to be, and the screen is a
viewport onto a part of it. A new window is added to the right of the focused one. Nothing is squeezed to make it fit:
the row gets longer, and the screen scrolls to follow your focus.

```json
{
  "monitors": [
    {
      "workspaces": [
        {
          "name": "I",
          "layout": "Scrolling",
          "layout_options": {
            "scrolling": {
              "columns": 3,
              "center_focused_column": "Never"
            }
          }
        }
      ]
    }
  ]
}
```

## Column widths

`columns` says how many columns fit across the screen, which is to say that a new column is a `columns`th of the screen
wide. It is `3` unless you say otherwise. It does not say how many windows there are: a lone window is a third of the
screen wide, and is not stretched to fill it.

- Opening or closing a window never changes the width of another window.
- A column that you have resized keeps its width as windows come and go.
- Columns that you have not resized follow `columns`. Changing it with
  [`komorebic scrolling-layout-columns`](../cli/scrolling-layout-columns.md), or crossing the threshold of a
  `layout_options_rules` entry, lays them out again.

## Centring the focused column

`center_focused_column` says when the focused column is put in the middle of the screen. It is one of:

- `"Never"`, the default, which scrolls only as far as it has to.
- `"Always"`, which keeps the focused column in the middle of the screen.
- `"OnOverflow"`, which centres it when it does not fit on the screen together with the column that focus came from.

`true` and `false` are also accepted, as they were before there were three choices: `true` is `"Always"` and `false` is
`"Never"`. Like `columns`, it is set for a workspace, or for a `layout_options_rules` entry.

A lone window is a third of the screen wide, at the left edge. To have it fill the screen, or to take it out of the
layout for a while, use [`komorebic toggle-monocle`](../cli/toggle-monocle.md).

## Scrolling

The row scrolls only when it has to.

- Focusing a window that is already on screen does not move anything.
- Focusing one that is not scrolls just far enough to bring it in, and no further.
- A window that is wider than the screen starts at the left edge.
- The focused window does not move when windows to its left open, close or change width.

When you close a window, focus goes back to the window that it was opened next to if you have not moved focus since,
and the screen scrolls back to where it was. If you have, focus goes to the window that takes the closed window's place
on the right, or to the one before it if there is none. A window that is closed without having focus does not take focus
with it.

## Resizing

[`komorebic resize-edge`](../cli/resize-edge.md) and [`komorebic resize-axis`](../cli/resize-axis.md) change the width of
the focused column, by half of the [resize delta](../cli/resize-delta.md) for each edge, so resizing the horizontal axis
changes the width by the whole delta. Dragging an edge with the mouse does the same. Columns are as tall as the screen,
so resizing a height does nothing.

## Windows that are out of view

A window that scrolls out of view comes to rest at the edge of the screen that it left by, with 5 pixels of it still on
screen. It keeps its size, so nothing has to be resized to bring it back. With [animations](animations.md) on, it slides
there.

A window that is partly in view keeps its size too, and overhangs the edge of the screen.

macOS shows a window on whichever display it overlaps. If another display is next to an edge, a window that rests there
or overhangs it would be seen on that display. In that case a window that has scrolled out of view is hidden in a corner
of the screen instead, and a window that is partly in view is made narrower to fit on the screen. If your displays are
arranged above and below each other rather than side by side, none of this applies.

## Limits

These are not done yet.

- A window that will not get as narrow as its column, which is what many apps do, is not noticed, so it can overlap the
  column next to it.
- There are no commands for stepping through preset widths, or for making a column the full width of the screen.
- Every window has a border, including the windows resting at the edges, so you may see a thin line of one at the edge
  of the screen.
