# Boot Visibility Notes

This project hides the window during fullscreen startup to avoid compositor
flashes. The boot flow is:

1. Create the window hidden.
2. Configure a safe fullscreen mode for warmup (borderless).
3. Render a small number of hidden warmup presents after a short settle delay.
4. Show the window only after warmup completes.
5. If the user requested exclusive fullscreen, apply it after the window
   becomes visible and immediately reconfigure the surface.

The goal is to avoid any "white quad" or OS-painted frames before the first
real content is ready. Windowed mode shows immediately after the first
successful render.
