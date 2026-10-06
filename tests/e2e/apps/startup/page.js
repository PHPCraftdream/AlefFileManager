// Startup scenario: a page with real content that quits by itself; `scenarios/startup-probe.ps1`
// watches the windows of the process and the first picture of the main window while it starts.
import { api } from './harness.js';

// Long enough for the probe to see the window shown and its first pictures.
setTimeout(() => api.app.quit(0), 4000);
