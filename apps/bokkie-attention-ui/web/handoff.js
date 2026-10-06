const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const STORAGE_KEY = 'bokkie-workspace-drafts-v1';

/** A return link only identifies a saved Bokkie snapshot; it never opens a project. */
export function handoffTarget(value, origin) {
  const url = new URL(value, origin);
  if (url.origin !== origin || url.pathname !== '/ui/') throw Error('Invalid hand-off return path');
  const id = url.searchParams.get('handoff');
  const raw = url.searchParams.get('revision');
  const revision = Number(raw);
  if (!UUID.test(id ?? '') || !/^[1-9][0-9]*$/.test(raw ?? '') || !Number.isSafeInteger(revision)) {
    throw Error('Invalid hand-off snapshot identity');
  }
  return [id, revision];
}

/** The environment isolates browser permission, storage and DOM fallback for tests. */
export function createHandoffBridge(env, repaint = () => {}) {
  let initialRead = false, link = null, prepared = null, copying = null;
  let gestureConsumed = false, results = [], saved = undefined;
  function queueLink(value) {
    const target = handoffTarget(value, env.location.origin);
    link = target;
    env.history.replaceState(null, '', `/ui/?handoff=${target[0]}&revision=${target[1]}`);
    repaint();
  }
  function beginCopy(gesture) {
    if (!gesture && gestureConsumed) { gestureConsumed = false; return; }
    if (!prepared || copying) return;
    gestureConsumed = gesture;
    const snapshot = {...prepared};
    // Call writeText synchronously from the trusted pointer gesture. Only its
    // resolved promise permits a successful clipboard activity report.
    let result;
    try {
      if (!env.clipboard?.writeText) throw Error('Clipboard unavailable');
      result = env.clipboard.writeText(snapshot.text);
    } catch (error) { result = Promise.reject(error); }
    copying = Promise.resolve(result).then(() => {
      results.push([snapshot.id, snapshot.revision, true]);
    }, () => {
      env.showFallback(snapshot.text);
      results.push([snapshot.id, snapshot.revision, false]);
    }).finally(() => { copying = null; repaint(); });
  }
  return {
    load() {
      try { const value = env.storage.getItem(STORAGE_KEY); return value?.length <= 262144 ? value : null; }
      catch { return null; }
    },
    store(value) {
      if (value === saved) return true;
      try { env.storage.setItem(STORAGE_KEY, value); saved = value; return true; }
      catch { return false; }
    },
    queueLink,
    takeLink() {
      if (!initialRead) {
        initialRead = true;
        if (!link && new URLSearchParams(env.location.search).has('handoff')) {
          try { link = handoffTarget(env.location.href, env.location.origin); } catch { /* Malformed links do not change conversation state. */ }
        }
      }
      const current = link; link = null;
      return current ? JSON.stringify(current) : null;
    },
    prepareCopy(value) {
      const v = JSON.parse(value);
      if (!UUID.test(v.id) || !Number.isSafeInteger(v.revision) || v.revision < 1 || typeof v.text !== 'string') throw Error('Invalid saved brief');
      prepared = v;
    },
    beginCopyGesture: () => beginCopy(true),
    copy: () => beginCopy(false),
    takeCopyResult() { const value = results.shift(); if (value) gestureConsumed = false; return value ? JSON.stringify(value) : null; },
  };
}

export function showSelectableBrief(document, text) {
  document.getElementById('bokkie-handoff-copy-fallback')?.remove();
  const panel = document.createElement('section');
  panel.id = 'bokkie-handoff-copy-fallback';
  panel.setAttribute('role', 'dialog');
  panel.setAttribute('aria-modal', 'true');
  panel.setAttribute('aria-labelledby', 'bokkie-handoff-copy-title');
  Object.assign(panel.style, {position:'fixed',inset:'8%',zIndex:'100',padding:'20px',background:'#20242b',color:'#f4f4f5',border:'1px solid #777',borderRadius:'6px',display:'flex',flexDirection:'column',gap:'12px',font:'14px "Bokkie Inter", sans-serif',boxShadow:'0 12px 60px #000b'});
  const title = document.createElement('h2'); title.id = 'bokkie-handoff-copy-title'; title.textContent = 'Copy complete saved brief';
  const explanation = document.createElement('p'); explanation.textContent = 'The browser could not confirm a clipboard copy. Select the full text below and copy it using your keyboard or device controls.';
  const field = document.createElement('textarea'); field.readOnly = true; field.value = text;
  field.setAttribute('aria-label','Complete saved hand-off brief');
  // eframe listens for clipboard events on the document even while this native
  // field is focused. Keep its canvas handler from cancelling native copying.
  for (const name of ['copy', 'cut', 'paste']) field.addEventListener(name, event => event.stopPropagation());
  Object.assign(field.style,{flex:'1',minHeight:'120px',width:'100%',boxSizing:'border-box',font:'14px "Bokkie Inter", sans-serif',padding:'12px',resize:'none',background:'#14181e',color:'#f4f4f5'});
  const close = document.createElement('button'); close.type = 'button'; close.textContent = 'Close text panel'; close.style.minHeight = '34px'; close.style.font = 'inherit';
  const originalFocus = document.activeElement;
  const dismiss = () => { panel.remove(); originalFocus?.focus?.(); };
  close.addEventListener('click',dismiss);
  panel.addEventListener('keydown',event => {
    if (event.key === 'Escape') { event.preventDefault(); dismiss(); }
    if (event.key === 'Tab') { event.preventDefault(); (document.activeElement === field ? close : field).focus(); }
  });
  panel.append(title,explanation,field,close); document.body.append(panel); field.focus(); field.select();
  return panel;
}
