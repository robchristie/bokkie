/** Finite physical-input conversation qualification against a marked synthetic DB. */
import { spawn, execFileSync } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, writeFile, readFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { randomUUID, createHash } from 'node:crypto';
import { chromium } from 'playwright';
const preflight = process.argv.includes('--preflight');
const root = process.cwd();
const evidence = resolve(process.env.BOKKIE_CONVERSATION_EVIDENCE ?? '.ui-qualification-runtime/conversation');
await mkdir(evidence, {recursive:true});
const profile = process.env.BOKKIE_CONVERSATION_PROFILE;
if (!preflight && !profile) throw Error('Live qualification requires an explicit private BOKKIE_CONVERSATION_PROFILE');
const fixtureRoot = process.env.BOKKIE_CONVERSATION_RESUME_ROOT ?? join('/tmp', `bokkie-conversation-journey-${randomUUID()}`);
const endpoint = process.env.BOKKIE_UI_LANTERN_ENDPOINT ?? 'http://127.0.0.1:9336';
const report = {mode:preflight?'no-model-preflight':'live-model',source:execFileSync('git',['rev-parse','HEAD'],{encoding:'utf8'}).trim(),budget:{calls:12,seconds:900,repair_calls:4,repair_seconds:300},calls:0,checks:[],errors:[],captures:[]};
for(const file of ['target/debug/bokkie-conversation-fixture','apps/bokkie-attention-ui/web/pkg/bokkie_attention_ui_bg.wasm','tools/conversation-runtime/broker.py']) report[file]=createHash('sha256').update(await readFile(file)).digest('hex');
let fixture,browser,page,origin,queue=[],pending=[],buffer='';
async function line(){if(queue.length)return queue.shift();return new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(Error('fixture reply timed out')),20000);pending.push(v=>{clearTimeout(timer);resolve(v);});});}
async function start(resume=false){
 fixture=spawn('target/debug/bokkie-conversation-fixture',['--root',fixtureRoot,'--ui-dir',resolve('apps/bokkie-attention-ui/web'),...(!preflight?['--profile',profile]:[]),...(resume?['--resume']:[])],{stdio:['pipe','pipe','pipe']});
 fixture.stderr.on('data',b=>report.errors.push(`fixture: ${b}`));
 fixture.stdout.on('data',b=>{buffer+=b;while(buffer.includes('\n')){const n=buffer.indexOf('\n'),v=JSON.parse(buffer.slice(0,n));buffer=buffer.slice(n+1);if(pending.length)pending.shift()(v);else queue.push(v);}});
 const initial=await line();origin=`http://${initial.address}`;report.initial=initial;
}
async function control(input={}){fixture.stdin.write(JSON.stringify(input)+'\n');const result=await line();if(result.error)throw Error(result.error);return result;}
async function stop(){if(fixture&&fixture.exitCode==null){fixture.stdin.end('{"stop":true}\n');await once(fixture,'exit');}fixture=undefined;}
const snapshot=()=>page.evaluate(()=>window.__BOKKIE_ATTENTION_HANDLE.test_snapshot());
async function ready(){await page.waitForFunction(()=>window.__BOKKIE_ATTENTION_HANDLE?.test_snapshot().interaction.connection==='current',null,{timeout:30000});}
async function reveal(id){
 for(let i=0;i<35;i++){
  const s=await snapshot(),n=s.ui_snapshot.nodes.find(n=>n.id===id),v=page.viewportSize();
  if(n && n.rect.min_y>=(["bokkie.conversation.open","bokkie.conversation.new"].includes(id)?0:60) && n.rect.max_y<v.height-12)return n;
  await page.mouse.move(v.width-30,v.height/2);await page.mouse.wheel(0,n&&n.rect.min_y<60?-400:400);await page.waitForTimeout(70);
 }
 throw Error(`Control not visible: ${id}`);
}
async function click(id){const n=await reveal(id);if(!n.enabled)throw Error(`Disabled control: ${id}`);await page.mouse.click((n.rect.min_x+n.rect.max_x)/2,(n.rect.min_y+n.rect.max_y)/2);await page.waitForTimeout(100);}
async function type(text){await click('bokkie.conversation.text');const ok=await page.evaluate(text=>{const i=document.activeElement;if(!(i instanceof HTMLInputElement))return false;i.value=text;i.dispatchEvent(new InputEvent('input',{bubbles:true,data:text,inputType:'insertText'}));return true;},text);if(!ok)throw Error('Browser IME agent not focused');await page.waitForTimeout(100);}
async function views(){const list=await (await fetch(origin+'/conversations')).json();return Promise.all(list.items.map(async i=>await(await fetch(origin+'/conversations/'+i.id)).json()));}
let current;
async function send(text){
 if(report.calls>=12)throw Error('Live model budget exhausted');
 await type(text);await click('bokkie.conversation.send');report.calls++;
 const started=Date.now();
 for(;;){const all=await views();const match=all.find(v=>v.messages.some(m=>m.role==='user'&&m.text===text));if(match&&!match.busy){current=match;if(match.request_error)throw Error(match.request_error);report.checks.push({text,view:match});await page.waitForTimeout(1200);return match;}
  if(Date.now()-started>110000)throw Error('Conversation turn exceeded finite bound');await page.waitForTimeout(400);
 }
}
async function confirm(){
 await click('bokkie.conversation.confirm');
 for(let i=0;i<40;i++){const v=await(await fetch(origin+'/conversations/'+current.id)).json();if(v.receipt&&v.receipt.command_id!==current.receipt?.command_id){current=v;report.checks.push({confirmation:v});await page.waitForTimeout(300);return v;}await page.waitForTimeout(150);}
 throw Error('Confirmation did not produce a new authoritative receipt');
}
function check(condition,message){if(!condition)throw Error(message);report.checks.push({passed:message});}
async function capture(name,focus){if(focus)await reveal(focus);await page.waitForTimeout(150);const s=await snapshot();await writeFile(join(evidence,name+'.json'),JSON.stringify(s,(_k,v)=>typeof v==="bigint"?v.toString():v,2));
 const targets=JSON.parse(execFileSync('lantern',['targets','--endpoint',endpoint,'--json'],{encoding:'utf8'}));const pages=(targets.targets??targets.result?.targets??[]).filter(t=>t.type==='page');if(pages.length!==1)throw Error('Owned browser target ambiguous');
 const args=['--endpoint',endpoint,'--target-id',pages[0].id??pages[0].target_id,'--json'];
 for(const [suffix,cmd]of [['layout',['layout','--container-selector','body']],['capture',['screenshot','--output',join(evidence,name+'.png'),'--overwrite']]]){const raw=execFileSync('lantern',[...cmd,...args],{encoding:'utf8'});const result=JSON.parse(raw);if(!result.ok)throw Error('Lantern '+suffix+' failed');await writeFile(join(evidence,name+'-'+suffix+'.json'),raw);}
 report.captures.push(name);
}
const deadline=setTimeout(()=>{report.errors.push('Aggregate time budget exceeded');fixture?.kill('SIGTERM');browser?.close();},900000);
try{
 await start(Boolean(process.env.BOKKIE_CONVERSATION_RESUME_ROOT));
 browser=await chromium.launch({headless:true,env:{...process.env,LD_LIBRARY_PATH:process.env.BOKKIE_UI_SYSROOT?`${resolve(process.env.BOKKIE_UI_SYSROOT,'usr/lib')}:${process.env.LD_LIBRARY_PATH??''}`:(process.env.LD_LIBRARY_PATH??'')},args:['--no-sandbox','--enable-unsafe-webgpu','--enable-features=Vulkan','--use-angle=vulkan','--disable-vulkan-surface',`--remote-debugging-port=${new URL(endpoint).port}`]});
 report.browser=browser.version();page=await browser.newPage({viewport:{width:1440,height:900}});page.on('pageerror',e=>report.errors.push(String(e)));
 await page.goto(origin+'/ui/');await ready();await click('bokkie.conversation.open');await page.waitForTimeout(500);
 if(preflight){
  await capture('preflight-desktop','bokkie.conversation.text');await page.setViewportSize({width:480,height:720});await capture('preflight-narrow','bokkie.conversation.text');const stats=await control({tick:true});check(!stats.ran&&stats.catalogue.items.length===0,'No-model UI preflight creates no work');
 }else{
  let v=await send("Help me set up a weekday reminder to review my research queue at 9 am Adelaide time. Don't activate it yet.");
  check(v.task?.status==='draft'&&v.task.runs.length===0,'Draft created without execution');const taskId=v.task.id;
  v=await send('Change the reminder text to: Review the research queue and choose one paper to read. Keep it inactive.');check(v.task.id===taskId&&!v.task.active,'Refinement preserves a single inactive task');
  v=await send('What exactly will happen? Preview it.');check(v.review.preview.definition.instructions==='Review the research queue and choose one paper to read.','Preview contains exact reminder text');check(v.review.preview.definition.trigger.timezone==='Australia/Adelaide'&&v.review.preview.occurrences.length>=3,'Preview shows named timezone and future occurrences');
  await capture('review-desktop','bokkie.conversation.confirm');await page.setViewportSize({width:480,height:720});await capture('review-narrow','bokkie.conversation.confirm');await page.setViewportSize({width:1440,height:900});
  v=await confirm();check(v.task.status==='active','Operator confirmation activates exact draft');const due=v.task.next_wake_at;
  let stats=await control({now:due,tick:true});check(stats.ran,'Kernel note runner completed due occurrence');stats=await control({tick:true});check(!stats.ran&&stats.details.find(t=>t.id===taskId).runs.filter(r=>r.result).length===1,'Repeated tick does not duplicate local result');
  const refresh=(await snapshot()).ui_snapshot.nodes.find(n=>n.actions.includes('refresh_operator_state'));if(!refresh)throw Error('Refresh control absent');await page.mouse.click((refresh.rect.min_x+refresh.rect.max_x)/2,(refresh.rect.min_y+refresh.rect.max_y)/2);await page.waitForTimeout(1200);
  const resultRun=stats.details.find(t=>t.id===taskId).runs.find(r=>r.result);await capture('completed-local-result','bokkie.conversation.result.'+resultRun.obligation_id);
  await stop();await start(true);await page.goto(origin+'/ui/');await ready();await click('bokkie.conversation.open');await page.waitForTimeout(300);
  v=await send('Find the research queue reminder.');check(v.candidates.some(c=>c.id===taskId),'Fresh conversation discovers persisted task');await click('bokkie.conversation.candidate.'+taskId);await page.waitForTimeout(250);
  v=await send('Make it Monday mornings instead, still at 9 am Adelaide time.');check(v.task.id===taskId&&v.task.active.definition.trigger.cron!==v.task.candidate.definition.trigger.cron,'Schedule proposal leaves active timing unchanged');await confirm();
  v=await send('Pause this reminder.');await confirm();const paused=await control();const before=paused.details.find(t=>t.id===taskId).runs.length;const monday=v.task.next_wake_at??due+7*86400;
  stats=await control({now:monday+86400,tick:true});check(!stats.ran&&stats.details.find(t=>t.id===taskId).runs.length===before,'Pause prevents new admission past a due time');
  v=await send('Resume this reminder.');v=await confirm();check(v.task.next_wake_at>stats.now,'Resume chooses future timing without backlog');
  await click('bokkie.conversation.new');await page.waitForTimeout(250);
  v=await send('Create a one-off local note now saying: Remember to organise the reading list.');await confirm();const onceId=current.task.id;stats=await control({tick:true});check(stats.ran&&stats.details.find(t=>t.id===onceId).status==='completed','One-off note completes through kernel');
  await page.reload();await ready();stats=await control({tick:true});check(!stats.ran&&stats.details.find(t=>t.id===onceId).runs.filter(r=>r.result).length===1,'Refresh does not repeat one-off completion');
  await click('bokkie.conversation.open');await page.waitForTimeout(250);await click('bokkie.conversation.new');await page.waitForTimeout(250);
  v=await send('Save a draft for an AI/ML research finder every weekday at 8 am Adelaide time, to find new papers relevant to my research queue.');check(v.task?.status==='draft'&&v.review?.blockers.length>0&&v.task.runs.length===0,'Unavailable research capability remains draft with activation blocked');await capture('unavailable-draft','bokkie.conversation.text');
  stats=await control();check(stats.catalogue.items.length===3,'Exactly three intended tasks exist after full journey');report.final=stats;
 }
 report.passed=true;
}catch(e){report.passed=false;report.errors.push(String(e.stack??e));if(page)await page.screenshot({path:join(evidence,'failure.png')}).catch(()=>{});throw e;
}finally{clearTimeout(deadline);if(browser)await browser.close();await stop();await writeFile(join(evidence,'qualification.json'),JSON.stringify(report,null,2));console.log(JSON.stringify({passed:report.passed,calls:report.calls,evidence,errors:report.errors}));}
