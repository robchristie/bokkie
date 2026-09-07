import initialise, { WebHandle } from "./pkg/bokkie_attention_ui.js";

await initialise();
const canvas = document.getElementById("bokkie-attention-canvas");
const handle = new WebHandle();
const appearance = new URLSearchParams(location.search).get("appearance") ?? "{}";
await handle.start_with_appearance(canvas, appearance);
document.getElementById("loading").remove();
canvas.dataset.bokkieReady = "true";
window.__BOKKIE_ATTENTION_HANDLE = handle;
