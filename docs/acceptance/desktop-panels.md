# Desktop panel resizing

The shell fits its viewport and allows grid/flex children to shrink. Wide download tables retain their own horizontal scroll. The status row uses its allocated grid row; compact navigation has reserved space. Optional thread details remain accessible in smaller windows and have their own close control.

Download columns use pointer capture with cleanup on release, cancellation, capture loss, Escape, focus loss, view change and unmount. A cancelled drag restores its previous width. Successful drags save against current column preferences. Separators also accept arrow keys, Home and End. Windows Edge checks covered commit/reload, all four cancellation paths, keyboard limits and no movement after a drag ends. Invalid non-finite stored widths are rejected.

Validation on 28 October 2026: frontend type checks, existing frontend tests and a production asset build passed. Fresh Windows Edge profiles passed an automated interaction run at 1366×768, 1024×600, 960×500, 800×600, 640×480, 390×500, 1280×400 and 1920×1080, including a return to the wide layout. Root scrolling stayed inside the window. Thread details could be opened, closed from within the pane, and reopened at narrow widths. Screenshots were inspected. These browser checks exercise the shared desktop CSS; installed Linux acceptance is not inferred from them.

The additional Windows Edge route matrix passed 7 workspace views at 1024×600, 800×600, 640×480 and 1280×400. Windows native builds and WebView2 acceptance are recorded separately when complete. The existing 0.0.1c downloads are unchanged by this source patch.
