import test from 'node:test';
import assert from 'node:assert/strict';
import {createHandoffBridge, handoffTarget} from './handoff.js';
const id = '10000000-0000-4000-8000-000000000001';
const origin = 'https://bokkie.example.test';
function fixture(clipboard) {
  const saved = new Map(), fallback = [], navigation = [];
  const env = {location:{origin,href:origin+'/ui/',search:''},history:{replaceState:(_state,_title,path)=>navigation.push(path)},
    storage:{getItem:key=>saved.get(key)??null,setItem:(key,value)=>saved.set(key,value)},clipboard,showFallback:text=>fallback.push(text)};
  return {env,saved,fallback,navigation,bridge:createHandoffBridge(env)};
}
const tick = () => new Promise(resolve=>setImmediate(resolve));
test('copy succeeds only after the browser clipboard promise resolves', async () => {
  let resolve, calls = 0; const f = fixture({writeText:()=>{calls++;return new Promise(r=>{resolve=r;});}});
  f.bridge.prepareCopy(JSON.stringify({id,revision:2,text:'Complete brief\nExact return link'}));
  f.bridge.beginCopyGesture(); f.bridge.copy();
  assert.equal(calls,1); assert.equal(f.bridge.takeCopyResult(),null);
  resolve(); await tick(); assert.deepEqual(JSON.parse(f.bridge.takeCopyResult()),[id,2,true]); assert.deepEqual(f.fallback,[]);
});
test('clipboard denial retains the complete selectable brief and reports failure honestly', async () => {
  const f = fixture({writeText:()=>Promise.reject(Error('permission denied'))});
  const text = 'Outcome\nContext\nConstraints\nAcceptance\nhttps://bokkie.example.test/ui/?handoff='+id+'&revision=1';
  f.bridge.prepareCopy(JSON.stringify({id,revision:1,text})); f.bridge.copy(); await tick();
  assert.deepEqual(f.fallback,[text]); assert.deepEqual(JSON.parse(f.bridge.takeCopyResult()),[id,1,false]);
});
test('return navigation pins a canonical same-origin snapshot without touching retained edits', () => {
  const f = fixture(); const value = JSON.stringify({unsent:'Keep my message',handoff:{editors:{draft:'Keep brief edits'}}});
  f.bridge.store(value); f.bridge.queueLink('/ui/?handoff='+id+'&revision=3');
  assert.deepEqual(JSON.parse(f.bridge.takeLink()),[id,3]); assert.equal(f.bridge.load(),value);
  assert.deepEqual(f.navigation,['/ui/?handoff='+id+'&revision=3']);
  const restarted = createHandoffBridge(f.env); assert.equal(restarted.load(),value);
});
test('initial return links work once and malformed/external links fail closed', () => {
  const f = fixture(); f.env.location.href = origin+'/ui/?handoff='+id+'&revision=4'; f.env.location.search='?handoff='+id+'&revision=4';
  assert.deepEqual(JSON.parse(f.bridge.takeLink()),[id,4]); assert.equal(f.bridge.takeLink(),null);
  for (const path of ['https://evil.test/ui/?handoff='+id+'&revision=1','/ui/?handoff=../../admin&revision=1','/ui/?handoff='+id+'&revision=0','/ui/?handoff='+id+'&revision=9007199254740992']) assert.throws(()=>handoffTarget(path,origin));
});

test('a settled pointer copy does not suppress a later keyboard copy', async () => {
  let calls=0; const f=fixture({writeText:()=>{calls++;return Promise.resolve();}});
  f.bridge.prepareCopy(JSON.stringify({id,revision:1,text:'Exact brief'}));
  f.bridge.beginCopyGesture();await tick();f.bridge.takeCopyResult();
  f.bridge.copy();await tick();assert.equal(calls,2);
  assert.deepEqual(JSON.parse(f.bridge.takeCopyResult()),[id,1,true]);
});
test('unavailable local storage is reported without discarding in-memory use', () => {
  const f=fixture();f.env.storage={getItem:()=>{throw Error('denied');},setItem:()=>{throw Error('denied');}};
  const bridge=createHandoffBridge(f.env);assert.equal(bridge.load(),null);assert.equal(bridge.store('retained draft'),false);
});
