// `stobie build-web` writes browser-startup.js, pkg/, manifest.webmanifest and
// sw.js beside this file. The loader registers sw.js because index.html links
// the manifest, and keeps #app-shell above an on-screen keyboard.
import { startApplication } from './browser-startup.js';

await startApplication({
  app: 'bokkie-web',
  load: () => import('./pkg/bokkie_web.js'),
  canvasId: 'app-canvas',
  handleName: '__STOBIE_HANDLE',
  start: (handle, canvas) => handle.start(canvas),
  fitViewport: true,
  afterStart: handle => {
    // With ?automation=1 in the URL, `stobie attach` can read and operate the page.
    if (new URL(location.href).searchParams.get('automation') === '1') {
      handle.enable_automation(crypto.randomUUID());
      window.__STOBIE_AUTOMATION = Object.freeze({
        request: json => handle.automation_request(json),
      });
    }
  },
});
