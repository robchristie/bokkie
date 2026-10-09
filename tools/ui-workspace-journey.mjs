/** Workspace UI/replay regression. Closed peers prove no live workspace delivery. */
import {spawn,execFileSync} from 'node:child_process';
import {once} from 'node:events';
import {mkdir,writeFile,readFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {randomUUID,randomBytes,createHash} from 'node:crypto';
import {chromium} from 'playwright';

const evidence=resolve(process.env.BOKKIE_WORKSPACE_EVIDENCE ?? '.ui-qualification-runtime/workspace');
await mkdir(evidence,{recursive:true});
const root=join('/tmp','bokkie-workspace-ui-'+randomUUID());
await mkdir(root,{mode:0o700});
const binary=process.env.BOKKIE_WORKSPACE_BINARY ?? resolve('target/debug/bokkie');
const project=randomUUID(), token=randomBytes(32).toString('hex');
const hostConfig=join(root,'hosts.json'),profile=join(root,'conversation.json');
await writeFile(hostConfig,JSON.stringify({hosts:[{id:'fixture',name:'Fixture host',token_sha256:createHash('sha256').update(token).digest('hex'),projects:[{project_id:project,profile_revision:'workspace-v1/fixture',permitted_actions:['inspect','source_changes','verify','reviewed_delivery'],limits:{max_seconds:7200,max_turns:4,max_tokens:1000000}}]}]}),{mode:0o600});
await writeFile(profile,JSON.stringify({broker:resolve('tests/fixtures/conversation_broker.py'),codex:'/usr/bin/true',bwrap:'/usr/bin/true',model:'fixture-workspace',effort:'medium',timezone:'Australia/Adelaide',timeout_seconds:30,max_context_bytes:65536,max_output_bytes:16384}),{mode:0o600});
const report={mode:'closed-model-and-host-peers',source:execFileSync('git',['rev-parse','HEAD'],{encoding:'utf8'}).trim(),checks:[],captures:[],errors:[]};
report.binary_sha256=createHash('sha256').update(await readFile(binary)).digest('hex');
let service,browser,page,origin,port=0,task,execution;
function check(value,message){if(!value)throw Error(message);report.checks.push(message);}
async function start(){
  service=spawn(binary,['--database',join(root,'controller.sqlite'),'serve','--bind',`127.0.0.1:${port}`,'--workspace-host-config',hostConfig,'--conversation-profile',profile,'--ui-dir',resolve('apps/bokkie-attention-ui/web')],{stdio:['ignore','pipe','pipe']});
  let buffer='';
  await new Promise((res,rej)=>{
    const timer=setTimeout(()=>rej(Error('Controller startup timed out')),20000);
    service.once('exit',code=>{clearTimeout(timer);rej(Error('Controller stopped during startup: '+code));});
    service.stderr.on('data',bytes=>{buffer+=bytes;while(buffer.includes('\n')){const end=buffer.indexOf('\n'),line=buffer.slice(0,end);buffer=buffer.slice(end+1);let event;try{event=JSON.parse(line);}catch{report.errors.push(line);continue;}if(event.event==='listening'){origin='http://'+event.address;port=Number(new URL(origin).port);clearTimeout(timer);res();}}});
  });
}
async function stop(){if(service?.exitCode==null){service.kill('SIGTERM');await once(service,'exit');}}
async function mutation(path,body){
  const boot=await(await fetch(origin+'/bootstrap')).json();
  const response=await fetch(origin+path,{method:'POST',headers:{'Content-Type':'application/json','X-Bokkie-Mutation-Token':boot.mutation_token},body:JSON.stringify(body)});
  if(!response.ok)throw Error(path+' '+response.status+' '+await response.text());
  return response.json();
}
async function host(events=[],heartbeats=[]){
  const response=await fetch(origin+'/workspace-hosts/fixture/exchange',{method:'POST',headers:{'Content-Type':'application/json','Authorization':'Bearer '+token},body:JSON.stringify({events,heartbeats})});
  if(!response.ok)throw Error('Host exchange '+response.status+' '+await response.text());
  return response.json();
}
const snapshot=()=>page.evaluate(()=>window.__BOKKIE_ATTENTION_HANDLE.test_snapshot());
async function ready(){await page.waitForFunction(()=>window.__BOKKIE_ATTENTION_HANDLE?.test_snapshot().interaction.connection==='current',null,{timeout:30000});await page.evaluate(()=>document.fonts.ready);}
async function visible(id){
  for(let i=0;i<70;i++){
    const s=await snapshot(),node=s.ui_snapshot.nodes.find(n=>n.id===id),h=page.viewportSize().height;
    if(node&&node.rect.max_y-node.rect.min_y>12&&node.rect.min_y>=0&&node.rect.max_y<h-4)return node;
    await page.mouse.move(page.viewportSize().width>1000?page.viewportSize().width-170:page.viewportSize().width/2,h/2);
    await page.mouse.wheel(0,i<45?240:-240);await page.waitForTimeout(70);
  }
  throw Error('Missing visible control '+id);
}
async function click(id){const n=await visible(id);check(n.enabled,'Enabled '+id);await page.mouse.click((n.rect.min_x+n.rect.max_x)/2,(n.rect.min_y+n.rect.max_y)/2);await page.waitForTimeout(120);}
async function input(id,text){
  await click(id);await page.keyboard.press('ControlOrMeta+A');
  const focused=await page.evaluate(text=>{const f=document.activeElement;if(!(f instanceof HTMLInputElement))return false;f.value=text;f.dispatchEvent(new InputEvent('input',{bubbles:true,data:text,inputType:'insertText'}));return true;},text);
  check(focused,'Text field focused '+id);await page.waitForTimeout(150);
}
async function capture(name){const path=join(evidence,name+'.png');await page.screenshot({path});report.captures.push({name,path,viewport:page.viewportSize()});}
async function rendered(id,label){await page.waitForFunction(({id,label})=>window.__BOKKIE_ATTENTION_HANDLE.test_snapshot().ui_snapshot.nodes.some(n=>n.id===id&&(!label||n.name.includes(label))),{id,label},{timeout:15000});}
async function waitView(id,predicate){for(let i=0;i<100;i++){const view=await(await fetch(origin+'/conversations/'+id)).json();if(predicate(view))return view;await page.waitForTimeout(100);}throw Error('Conversation postcondition not reached');}

try{
  await start();
  await mutation('/projects',{command_id:randomUUID(),project_id:project,expected_revision:0,registration:{name:'Atlas',host:'Fixture host',workspace:'/development/atlas-workspace',context:'Synthetic documentation project',codex_project_id:null,codex_host_id:null}});
  browser=await chromium.connectOverCDP(process.env.BOKKIE_UI_LANTERN_ENDPOINT ?? 'http://127.0.0.1:9352');
  const context=await browser.newContext({viewport:{width:1440,height:1000}});
  page=await context.newPage();page.on('pageerror',error=>report.errors.push(error.message));
  await page.goto(origin+'/ui/');await ready();
  await input('bokkie.conversation.text','Create a workspace task for Atlas to keep documentation aligned with code.');
  const sent=page.waitForRequest(r=>r.method()==='POST'&&r.url().endsWith('/conversations/turn'));await click('bokkie.conversation.send');
  const request=(await sent).postDataJSON();
  let view=await waitView(request.conversation_id,v=>!v.busy&&v.review);
  check(!view.request_error,'Workspace conversation draft saved');task=view.task.id;
  await rendered('bokkie.conversation.confirm');
  await capture('desktop-review');await click('bokkie.conversation.confirm');
  view=await waitView(view.id,v=>v.task?.status==='active');
  // Discard the first admission response, then recover that same durable intent.
  await host();const recovered=await host();check(recovered.dispatches.length===1,'One dispatch after lost acknowledgement');
  const dispatch=recovered.dispatches[0];execution=dispatch.execution_id;
  const events=[
    {execution_id:execution,sequence:1,event:{kind:'started',runtime_id:'closed-peer',instruction_sources:['/synthetic/AGENTS.md']}},
    {execution_id:execution,sequence:2,event:{kind:'progress',summary:'Inspecting the agreed documentation scope'}},
    {execution_id:execution,sequence:3,event:{kind:'question',question:{id:'which-section',kind:'missing_information',prompt:'Which section should receive the correction?',options:['Existing operator guide','Contributor guide']}}},
  ];
  await host(events,[execution]);const replay=await host(events,[execution]);check(replay.acknowledgements.some(a=>a.execution_id===execution&&a.sequence===3),'Events replay without duplicate questions');
  await page.reload();await ready();await page.waitForTimeout(700);
  await capture('desktop-question');
  await click('bokkie.workspace.answer.'+execution);await input('bokkie.workspace.answer-text','Existing operator guide');await click('bokkie.workspace.run-confirm');
  const answered=await host([], [execution]);check(answered.controls.some(c=>c.execution_id===execution&&c.answers.length===1),'One durable answer available to host');
  await click('bokkie.task.edit');await input('bokkie.task-editor.scope','Revised future scope: documentation only; preserve reviewed outcomes.');await click('bokkie.task-editor.save');
  view=await waitView(view.id,v=>v.task?.candidate?.revision===2);
  check(view.task.active.revision===1,'Direct editing creates a candidate without redirecting active definition');
  await page.setViewportSize({width:390,height:844});await page.reload();await ready();await page.waitForTimeout(600);await capture('narrow-candidate');
  await context.close();await stop();await start();
  const after=await host(events,[execution]);
  check(after.dispatches.length===1&&after.dispatches[0].execution_id===execution,'Controller restart retains original execution');
  check(after.dispatches[0].assignment.brief.constraints===dispatch.assignment.brief.constraints,'Admitted scope remains pinned after direct editing');
  check(after.controls.some(c=>c.answers.length===1),'Answer survives browser closure and controller restart');
  const newContext=await browser.newContext({viewport:{width:1440,height:1000}});page=await newContext.newPage();await page.goto(origin+'/ui/');await ready();
  view=await(await fetch(origin+'/conversations/'+view.id)).json();
  await mutation('/conversations/select',{command_id:randomUUID(),conversation_id:view.id,expected_revision:view.revision,task_id:task});
  await page.goto(origin+'/ui/?task='+encodeURIComponent(task));await ready();await page.waitForTimeout(600);
  await click('bokkie.workspace.stop.'+execution);await click('bokkie.workspace.run-confirm');
  const cancelling=await host();check(cancelling.controls.some(c=>c.cancel),'Cancellation intent reaches retained execution');
  await host([{execution_id:execution,sequence:4,event:{kind:'stopped',cessation:{boundary_id:'synthetic-boundary',kind:'not_started',evidence:'Closed peer started no process'},result:null,verification:null,reason:'Synthetic cancellation reconciled'}}]);
  view=await waitView(view.id,v=>v.task?.runs[0].state==='cancelled');
  check(view.task.runs.length===1,'Cancellation does not create another immediate job');
  await rendered('bokkie.workspace.run-heading.'+execution,'Cancelled');
  await page.waitForTimeout(400);await capture('desktop-cancelled');await newContext.close();
  report.success=true;
}catch(error){report.success=false;report.failure=error.stack;if(page)await capture('failure').catch(()=>{});process.exitCode=1;}
finally{await stop();await browser?.close();await writeFile(join(evidence,'report.json'),JSON.stringify(report,null,2));console.log(JSON.stringify({success:report.success,checks:report.checks.length,captures:report.captures.length,evidence,failure:report.failure}));}
