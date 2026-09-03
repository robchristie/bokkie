import initialise, { WebHandle } from "./pkg/bokkie_attention_ui.js";

await initialise();
const canvas = document.getElementById("bokkie-attention-canvas");
const handle = new WebHandle();
await handle.start(canvas);
document.getElementById("loading").remove();
canvas.dataset.bokkieReady = "true";
window.__BOKKIE_ATTENTION_HANDLE = handle;
