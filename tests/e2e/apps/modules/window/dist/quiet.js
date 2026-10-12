// The document of a window created by the main page: it takes over close requests and never
// answers them, so the runtime has to close the window when its time limit is over. It tells the
// main page that it is ready by retitling its window.
import { api, guard, report } from './harness.js';

guard(async () => {
  const self = await api.window.current();
  await self.on('close-requested', () => new Promise(() => {}));
  await self.setTitle('armed');
  await report(`quiet-armed ${self.label}`);
});
