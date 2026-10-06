/** Finite physical-input hand-off regression with a closed synthetic model peer. */
import {spawn, execFileSync} from 'node:child_process';
import {once} from 'node:events';
import {mkdir, writeFile, readFile} from 'node:fs/promises';
import {resolve, join} from 'node:path';
import {randomUUID, createHash} from 'node:crypto';
import {chromium} from 'playwright';
const evidence = resolve(process.env.BOKKIE_HANDOFF_EVIDENCE ?? '.ui-qualification-runtime/handoff');
await mkdir(evidence,{recursive:true});
const root = join('/tmp',`bokkie-handoff-${randomUUID()}`);
const profile = join(evidence,'synthetic-profile.json');
await writeFile(profile,JSON.stringify({broker:resolve('tests/fixtures/conversation_broker.py'),codex:'/usr/bin/true',bwrap:'/usr/bin/true',model:'fixture-handoff',effort:'medium',timezone:'Australia/Adelaide',timeout_seconds:30,max_context_bytes:65536,max_output_bytes:16384}));
const endpoint = process.env.BOKKIE_UI_LANTERN_ENDPOINT ?? 'http://127.0.0.1:9352';
const report = {mode:'deterministic-model-peer',source:execFileSync('git',['rev-parse','HEAD'],{encoding:'utf8'}).trim(),started_at:new Date().toISOString(),checks:[],captures:[],errors:[],network:[],console:[]};
for (const file of ['target/debug/bokkie-conversation-fixture','apps/bokkie-attention-ui/web/pkg/bokkie_attention_ui_bg.wasm','tests/fixtures/conversation_broker.py','apps/bokkie-attention-ui/assets/fonts/Inter-Regular.ttf']) report[file] = createHash('sha256').update(await readFile(file)).digest('hex');
let fixture, browser, page, origin, port=0, buffer='', queue=[], pending=[], callBoundary;
function line() {
 if(queue.length)return Promise.resolve(queue.shift());
 return new Promise((res,rej)=>{const timer=setTimeout(()=>rej(Error('Fixture response timed out')),20000);pending.push(v=>{clearTimeout(timer);res(v);});});
}
async function start(resume=false) {
 fixture=spawn('target/debug/bokkie-conversation-fixture',['--root',root,'--port',String(port),'--profile',profile,'--ui-dir',resolve('apps/bokkie-attention-ui/web'),...(resume?['--resume']:[])],{stdio:['pipe','pipe','pipe']});
 fixture.stderr.on('data',data=>report.errors.push(String(data)));
 fixture.stdout.on('data',data=>{buffer+=data;while(buffer.includes('\n')){const n=buffer.indexOf('\n'),value=JSON.parse(buffer.slice(0,n));buffer=buffer.slice(n+1);if(pending.length)pending.shift()(value);else queue.push(value);}});
 const initial=await line();origin=`http://${initial.address}`;port=Number(new URL(origin).port);report.fixture=initial;
}
async function control(value={}) { fixture.stdin.write(JSON.stringify(value)+'\n');return line(); }
async function stop() { if(fixture?.exitCode==null){fixture.stdin.end('{"stop":true}\n');await once(fixture,'exit');} }
function check(value,message) { if(!value)throw Error(message);report.checks.push(message); }
const snapshot = () => page.evaluate(()=>window.__BOKKIE_ATTENTION_HANDLE.test_snapshot());
async function ready() {
 await page.waitForFunction(()=>window.__BOKKIE_ATTENTION_HANDLE?.test_snapshot().interaction.connection==='current',null,{timeout:30000});
 const faces=await page.evaluate(async()=>{const loaded=await document.fonts.load('14px "Bokkie Inter"');await document.fonts.ready;return loaded.map(face=>({family:face.family,status:face.status}));});
 check(faces.length===1&&faces[0].status==='loaded','Browser reading face is loaded before mutation fault injection');
}
async function reveal(id) {
 for(let i=0;i<55;i++) {
  const s=await snapshot(), n=s.ui_snapshot.nodes.find(n=>n.id===id), height=page.viewportSize().height;
  if(n && n.rect.max_y-n.rect.min_y>16 && n.rect.min_y>=0 && n.rect.max_y<=height-4)return n;
  await page.mouse.move(page.viewportSize().width/2, Math.min(height-230,height/2));
  await page.mouse.wheel(0,i<30?220:-220);await page.waitForTimeout(65);
 }
 throw Error(`Missing visible control ${id}`);
}
async function click(id) { const n=await reveal(id);if(!n.enabled)throw Error(`Disabled ${id}`);await page.mouse.click((n.rect.min_x+n.rect.max_x)/2,(n.rect.min_y+n.rect.max_y)/2);await page.waitForTimeout(100); }
async function input(id,value) {
 await click(id);await page.keyboard.press('ControlOrMeta+A');await page.waitForTimeout(80);
 const ok=await page.evaluate(value=>{const field=document.activeElement;if(!(field instanceof HTMLInputElement))return false;field.value=value;field.dispatchEvent(new InputEvent('input',{bubbles:true,data:value,inputType:'insertText'}));return true;},value);
 if(!ok)throw Error(`Browser text agent did not focus ${id}`);await page.waitForTimeout(130);
}
async function mutation(path,body) {
 const boot=await(await fetch(origin+'/bootstrap')).json();const r=await fetch(origin+path,{method:'POST',headers:{'Content-Type':'application/json','X-Bokkie-Mutation-Token':boot.mutation_token},body:JSON.stringify(body)});
 if(!r.ok)throw Error(`Fixture setup ${path} failed ${r.status}: ${await r.text()}`);return r.json();
}
async function send(text) {
 await input('bokkie.conversation.text',text);
 await page.waitForFunction(()=>window.__BOKKIE_ATTENTION_HANDLE.test_snapshot().ui_snapshot.nodes.some(n=>n.id==='bokkie.conversation.send'&&n.enabled),null,{timeout:15000});
 const request=page.waitForRequest(r=>r.method()==='POST'&&r.url().endsWith('/conversations/turn'));request.catch(()=>{});await click('bokkie.conversation.send');const submitted=(await request).postDataJSON();
 for(let i=0;i<80;i++) {
  const v=await(await fetch(origin+'/conversations/'+submitted.conversation_id)).json();
  if(!v.busy&&v.messages.some(m=>m.request_id===submitted.command_id)){check(!v.request_error,'Synthetic turn completed without runtime error');await page.waitForTimeout(300);return v;}
  await page.waitForTimeout(100);
 }
 throw Error('Synthetic conversation turn did not finish');
}
async function submitted(path,action) {
 const response=page.waitForResponse(r=>r.request().method()==='POST'&&r.url().endsWith(path));response.catch(()=>{});await action();const r=await response;if(!r.ok())throw Error(`Mutation ${path} failed ${r.status()}: ${await r.text()}`);check(true,`Mutation ${path} accepted`);return r.json();
}
async function capture(name) {
 await page.waitForTimeout(200);const current=await snapshot();
 await writeFile(join(evidence,name+'.json'),JSON.stringify(current,(_key,value)=>typeof value==='bigint'?value.toString():value,2));
 const targets=JSON.parse(execFileSync('lantern',['targets','--endpoint',endpoint,'--json'],{encoding:'utf8'})).targets.filter(t=>t.type==='page');
 check(targets.length===1,'Owned disposable browser target is unambiguous');
 const args=['--endpoint',endpoint,'--target-id',targets[0].id,'--json'];
 for(const [suffix,command] of [['layout',['layout','--container-selector','body']],['capture',['screenshot','--output',join(evidence,name+'.png'),'--overwrite']]]) {
  const raw=execFileSync('lantern',[...command,...args],{encoding:'utf8'});check(JSON.parse(raw).ok,`Lantern ${suffix} collected`);await writeFile(join(evidence,name+'-'+suffix+'.json'),raw);
 }
 report.captures.push({name,viewport:page.viewportSize()});
}
async function noExecution(message) { const c=await control();check(c.model_calls===callBoundary,message);check(c.catalogue.items.length===0,'Hand-off activity creates no execution task'); }
const guard=setTimeout(()=>{fixture?.kill('SIGTERM');browser?.close();},240000);
try {
 await start();
 const servedFont=await fetch(origin+'/ui/fonts/Inter-Regular.ttf');
 check(servedFont.ok,'Licensed web fallback font is served by the same Bokkie origin');
 report.served_font_sha256=createHash('sha256').update(Buffer.from(await servedFont.arrayBuffer())).digest('hex');
 check(report.served_font_sha256===report['apps/bokkie-attention-ui/assets/fonts/Inter-Regular.ttf'],'Served web font matches the authoritative licensed Inter input');
 browser=await chromium.launch({headless:true,env:{...process.env,LD_LIBRARY_PATH:`${resolve(process.env.BOKKIE_UI_SYSROOT??'/nvme/development/polyorama/.tools/sysroot','usr/lib')}:${process.env.LD_LIBRARY_PATH??''}`},args:['--no-sandbox','--enable-unsafe-webgpu','--enable-features=Vulkan','--use-angle=vulkan','--disable-vulkan-surface',`--remote-debugging-port=${new URL(endpoint).port}`]});
 report.browser=browser.version();page=await browser.newPage({viewport:{width:1440,height:900}});
 page.on('console',m=>{if(['error','warning'].includes(m.type()))report.console.push({kind:m.type(),text:m.text().slice(0,1500)});});page.on('pageerror',e=>report.errors.push(String(e)));page.on('response',r=>{if(r.url().includes('/fonts/'))report.network.push({path:new URL(r.url()).pathname,status:r.status(),content_type:r.headers()['content-type']});});page.on('requestfailed',r=>report.network.push({path:new URL(r.url()).pathname,error:r.failure()?.errorText}));
 await page.goto(origin+'/ui/');await ready();

 await input('bokkie.conversation.text','Unsent discussion retained through project registration');
 await click('bokkie.settings.open');await click('bokkie.settings.projects');
 await click('bokkie.handoff.project-new');await input('bokkie.handoff.project-name','Atlas');await input('bokkie.handoff.project-host','Nostromo');await input('bokkie.handoff.project-workspace','/synthetic/nostromo/atlas');await input('bokkie.handoff.project-context','Synthetic web application on Nostromo.');
 const registered=await submitted('/projects',()=>click('bokkie.handoff.project-save'));const first=registered.items.find(p=>p.registration.host==='Nostromo');check(!!first,'Physical Settings registration saves the first existing workspace');
 const secondId=randomUUID();await mutation('/projects',{command_id:randomUUID(),project_id:secondId,expected_revision:0,registration:{name:'Atlas',host:'Sulaco',workspace:'/synthetic/sulaco/atlas',codex_project_id:null,codex_host_id:null,context:'Synthetic alternate application on Sulaco.'}});
 await click('bokkie.handoff.refresh');await page.waitForTimeout(200);await capture('project-workspaces-desktop');
 await click('bokkie.handoff.return');
 const conversationRead=JSON.parse(await page.evaluate(()=>window.__BOKKIE_HANDOFF.load()));check(conversationRead.unsent==='Unsent discussion retained through project registration','Settings and project navigation retain the unsent composer');
 await send('Let’s discuss a searchable project list for Atlas. Keep the current selection when searching.');
 const draft=await send('Prepare a hand-off for Atlas to add a searchable project list with Australian English labels.');check(draft.handoff_draft.candidates.length===2,'Backend presents two ambiguous similarly named projects on distinct hosts');
 callBoundary=(await control()).model_calls;
 await input('bokkie.conversation.text','Unsent follow-up must survive opening the saved hand-off');
 await click('bokkie.conversation.handoff');
 check(!(await reveal('bokkie.handoff.save')).enabled,'Ambiguous workspace blocks saving until explicit project selection');await capture('ambiguous-draft-desktop');
 await click('bokkie.handoff.choose.'+secondId);await input('bokkie.handoff.context','Retain the current selection during search. Keep existing project navigation. Edited in the brief.');
 await page.setViewportSize({width:390,height:844});await page.waitForTimeout(250);await capture('edited-draft-narrow');
 await page.reload();await ready();await click('bokkie.handoffs.open');await click('bokkie.handoff.history');await click('bokkie.handoff.draft.'+draft.handoff_draft.id);
 check((await reveal('bokkie.handoff.save')).enabled,'Browser refresh preserves the explicit destination and entered brief');
 const local=JSON.parse(await page.evaluate(()=>window.__BOKKIE_HANDOFF.load()));check(local.handoff.editors[draft.handoff_draft.id].draft.brief.context.includes('Edited in the brief'),'Reload preserves unsaved brief edits');check(local.unsent==='Unsent follow-up must survive opening the saved hand-off','Reload retains unsent conversational text');
 let saveBody;
 await page.route('**/handoffs/save', async route => {
  saveBody=route.request().postDataJSON();
  const accepted=await route.fetch();check(accepted.ok(),'Injected lost response follows an accepted server save');
  await route.abort('failed');
 }, {times:1});
 await click('bokkie.handoff.save');
 await page.waitForFunction(()=>window.__BOKKIE_ATTENTION_HANDLE.test_snapshot().ui_snapshot.nodes.some(n=>n.id==='bokkie.handoff.retry'&&n.enabled));
 const uncertainLocal=JSON.parse(await page.evaluate(()=>window.__BOKKIE_HANDOFF.load()));
 check(uncertainLocal.handoff.pending.Brief.command_id===saveBody.command_id,'Lost response durably retains the exact original mutation envelope');
 await capture('uncertain-save-narrow');
 const retried=page.waitForRequest(r=>r.method()==='POST'&&r.url().endsWith('/handoffs/save'));retried.catch(()=>{});
 const saved=await submitted('/handoffs/save',()=>click('bokkie.handoff.retry'));
 check(JSON.stringify((await retried).postDataJSON())===JSON.stringify(saveBody),'Operator retry sends the same request ID and every original field');
 check(saved.snapshot.project.id===secondId&&saved.snapshot.revision===1,'Explicit workspace and edited brief are saved as one exact revision');check(saved.snapshot.complete_brief.includes('Edited in the brief')&&saved.snapshot.complete_brief.includes(saved.snapshot.return_path),'Complete saved brief includes the edited context and exact return path');
 await capture('saved-brief-narrow');
 const repeated=await mutation('/handoffs/save',{...saveBody,command_id:randomUUID()});check(repeated.snapshot.revision===1,'Repeating an unchanged save reuses the immutable snapshot');
 await noExecution('Registration, reads, edits and repeated saves dispatch no additional model');
 await page.setViewportSize({width:1440,height:900});await page.waitForTimeout(200);
 await page.context().grantPermissions(['clipboard-read','clipboard-write'],{origin});
 const copiedSuccess=await submitted('/handoffs/activity',()=>click('bokkie.handoff.copy'));
 check(copiedSuccess.activities.some(a=>a.kind==='copy_succeeded'),'Resolved browser clipboard write records reported copy success');
 check(await page.evaluate(()=>navigator.clipboard.readText())===saved.snapshot.complete_brief,'Browser clipboard contains the complete exact saved brief');
 await capture('saved-brief-desktop');
 await page.evaluate(()=>navigator.clipboard.writeText('Synthetic manual-copy sentinel'));
 await page.evaluate(()=>{navigator.clipboard.writeText=()=>Promise.reject(Error('synthetic clipboard permission denial'));});
 const copied=await submitted('/handoffs/activity',()=>click('bokkie.handoff.copy'));check(copied.activities.some(a=>a.kind==='copy_failed'),'Clipboard denial records failure rather than a requested-copy success');
 await page.waitForSelector('#bokkie-handoff-copy-fallback textarea');await page.evaluate(async()=>{await document.fonts.load('14px "Bokkie Inter"');await document.fonts.ready;});check(await page.evaluate(()=>document.fonts.check('14px "Bokkie Inter"')),'DOM copy fallback has loaded the product reading face');check(await page.locator('#bokkie-handoff-copy-fallback textarea').inputValue()===saved.snapshot.complete_brief,'Selectable fallback contains every byte of the complete saved brief');check(await page.locator('#bokkie-handoff-copy-fallback textarea').evaluate(field=>field.readOnly&&field.selectionStart===0&&field.selectionEnd===field.value.length),'Fallback full text is selected in a read-only native browser field');
 await page.keyboard.press('ControlOrMeta+C');await page.waitForTimeout(150);check(await page.evaluate(()=>navigator.clipboard.readText())===saved.snapshot.complete_brief,'Physical keyboard copying from the selectable fallback replaces the clipboard sentinel with the complete brief');
 await capture('clipboard-fallback-desktop');await page.setViewportSize({width:390,height:844});await page.waitForTimeout(200);await capture('clipboard-fallback-narrow');await page.getByRole('button',{name:'Close text panel'}).click();await page.setViewportSize({width:1440,height:900});await page.waitForTimeout(200);
 const opening=await submitted('/handoffs/activity',()=>click('bokkie.handoff.opening'));check(opening.activities.some(a=>a.kind==='manual_opening_viewed'),'Opening guidance records an explicit manual-view action');
 await reveal('bokkie.handoff.note');await capture('manual-opening-desktop');await noExecution('Copy fallback and manual opening guidance dispatch no model or execution task');
 await input('bokkie.handoff.note','Synthetic opening problem: the registered workspace is missing on Sulaco.');
 const problem=await submitted('/handoffs/activity',()=>click('bokkie.handoff.problem'));
 const reported=problem.activities.find(a=>a.kind==='opening_problem_reported');
 check(reported?.note.includes('workspace is missing')&&reported.provenance==='Operator-entered report; not independently verified','Opening failure remains an operator-entered, unverified report');
 check(!('execution_acknowledgement' in problem)&&!('execution_status' in problem.snapshot),'Opening failure does not create an execution acknowledgement');
 await noExecution('Reporting an opening failure dispatches no model or execution task');
 await input('bokkie.handoff.note','Operator observed the searchable list in the selected Sulaco workspace.');const result=await submitted('/handoffs/activity',()=>click('bokkie.handoff.result'));check(result.activities.some(a=>a.kind==='result_note'&&a.note.includes('Operator observed')),'Result note is saved as operator-entered observation');
 await reveal('bokkie.handoff.activity.'+(result.activities.length-1));await page.waitForFunction(()=>window.__BOKKIE_ATTENTION_HANDLE.test_snapshot().ui_snapshot.nodes.some(n=>n.id.startsWith('bokkie.handoff.activity.')&&n.name.includes('Operator observed')));await capture('operator-result-desktop');
 await input('bokkie.handoff.note','Keep this unfinished result note through restart');await page.setViewportSize({width:390,height:844});await page.waitForTimeout(200);await reveal('bokkie.handoff.note');await capture('manual-result-narrow');
 await click('bokkie.handoff.return');await page.waitForTimeout(350);await click('bokkie.conversation.handoff');await page.waitForFunction(()=>window.__BOKKIE_ATTENTION_HANDLE.test_snapshot().ui_snapshot.nodes.some(n=>n.id==='bokkie.handoff'));check(!(await snapshot()).ui_snapshot.nodes.some(n=>n.id==='bokkie.handoff.save'),'Home opens the saved snapshot rather than resurrecting the original model draft');await click('bokkie.handoff.return');await page.evaluate(path=>window.__BOKKIE_HANDOFF.queueLink(path),saved.snapshot.return_path);await page.waitForTimeout(400);await capture('return-link-narrow');
 check(JSON.parse(await page.evaluate(()=>window.__BOKKIE_HANDOFF.load())).unsent==='Unsent follow-up must survive opening the saved hand-off','Same-app return navigation retains the unsent composer');
 await stop();await start(true);await page.reload();await ready();await page.waitForTimeout(250);
 const retained=await(await fetch(origin+'/handoffs/'+saved.snapshot.id+'?revision='+saved.snapshot.revision)).json();check(retained.snapshot.complete_brief===saved.snapshot.complete_brief,'Service restart retains the exact immutable saved brief');check(retained.activities.some(a=>a.kind==='result_note'),'Service restart retains operator-entered result provenance');
 const localAfter=JSON.parse(await page.evaluate(()=>window.__BOKKIE_HANDOFF.load()));check(localAfter.handoff.note==='Keep this unfinished result note through restart','Return link and service restart retain an unfinished result note');await capture('restart-return-narrow');
 await noExecution('Return navigation and restart trigger only reads');report.passed=true;
} catch(error) {
 report.passed=false;report.errors.push(String(error.stack??error));if(page){await writeFile(join(evidence,'failure.json'),JSON.stringify(await snapshot(),(_k,v)=>typeof v==='bigint'?v.toString():v,2)).catch(()=>{});await page.screenshot({path:join(evidence,'failure.png')}).catch(()=>{});}throw error;
} finally {
 clearTimeout(guard);report.finished_at=new Date().toISOString();if(fixture?.exitCode==null){try{report.dispatches=(await control()).model_calls;}catch{}}
 if(browser)await browser.close();await stop();await writeFile(join(evidence,'qualification.json'),JSON.stringify(report,null,2));console.log(JSON.stringify({passed:report.passed,evidence,dispatches:report.dispatches,errors:report.errors}));
}
