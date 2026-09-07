import { createHash } from 'node:crypto';

const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const semanticSignature = state => JSON.stringify({
  nodes: state.ui_snapshot.nodes,
  text: state.ui_snapshot.text,
  interaction: state.interaction,
}, (_key, value) => typeof value === 'bigint' ? value.toString() : value);

// A ready Rust model does not mean the browser has finished painting a window's
// fade-in or settling its geometry. Request a real canvas repaint and require two
// consecutive identical framebuffers and semantic layouts before retaining them.
export async function captureSettled(page, snapshot) {
  let previous;
  const started = Date.now();
  for (let sample = 1; sample <= 16; sample += 1) {
    const before = await snapshot(page);
    // The UI intentionally stays idle when blank-canvas movement changes nothing.
    // Explicit repaint advances the actual animation clock without fake input.
    await page.evaluate(() => window.__BOKKIE_ATTENTION_HANDLE.request_repaint());
    await page.waitForFunction(frame => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot().frame_number > frame,
      before.frame_number, { timeout: 5_000 });
    await page.waitForTimeout(100);
    const state = await snapshot(page);
    const png = await page.screenshot();
    const after = await snapshot(page);
    const pixels = digest(png);
    const semantic = semanticSignature(state);
    const sameFrame = state.frame_number === after.frame_number;
    if (sameFrame && previous?.pixels === pixels && previous.semantic === semantic) {
      return { state, png, settling: {
        samples: sample, elapsed_ms: Date.now() - started,
        frame: state.frame_number, png_sha256: pixels,
        criterion: 'two consecutive identical framebuffers and semantic layouts; unchanged frame across final capture',
      } };
    }
    previous = sameFrame ? { pixels, semantic } : undefined;
  }
  throw new Error('application framebuffer and semantic layout did not settle within 16 repaint probes');
}
