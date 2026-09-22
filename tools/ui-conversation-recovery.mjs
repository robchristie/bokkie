/** Zero-model browser regression using retained synthetic real-conversation history. */
import {spawn,execFileSync} from 'node:child_process';
import {once} from 'node:events';
import {readFile,writeFile,mkdir} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {createHash} from 'node:crypto';
import {chromium} from 'playwright';
const source=process.env.BOKKIE_CONVERSATION_RECOVERY_SOURCE;
if(!source)throw Error('Provide the retained synthetic journey report');
const prefixBytes=await readFile(source),prefix=JSON.parse(prefixBytes);
if(prefix.mode!=='live-model'||!prefix.passed||prefix.initial.database_kind!=='synthetic_fixture')throw Error('Requires a successful synthetic live-model journey');
const evidence=resolve(process.env.BOKKIE_CONVERSATION_EVIDENCE??'.ui-qualification-runtime/conversation-recovery');
await mkdir(evidence,{recursive:true});
const report={source:execFileSync('git',['rev-parse','HEAD'],{encoding:'utf8'}).trim(),mode:'zero-model-read-recovery',prefix_sha256:createHash('sha256').update(prefixBytes).digest('hex'),checks:[],passed:false};
for(const file of ['target/debug/bokkie-conversation-fixture','apps/bokkie-attention-ui/web/pkg/bokkie_attention_ui_bg.wasm'])report[file]=createHash('sha256').update(await readFile(file)).digest('hex');
let browser,child,buffer='',queue=[],pending=[],page;
const line=()=>queue.length?Promise.resolve(queue.shift()):new Promise((ok,fail)=>{const timer=setTimeout(()=>fail(Error('Fixture control deadline')),10000);pending.push(v=>{clearTimeout(timer);ok(v);});});
async function control(){child.stdin.write('{}\n');return line();}
function check(ok,text){if(!ok)throw Error(text);report.checks.push(text);}
const deadline=setTimeout(()=>{child?.kill();browser?.close();},60000);
try{
 child=spawn('target/debug/bokkie-conversation-fixture',['--root',prefix.initial.root,'--resume','--ui-dir',resolve('apps/bokkie-attention-ui/web')],{stdio:['pipe','pipe','pipe']});
 child.stdout.on('data',bytes=>{buffer+=bytes;while(buffer.includes('\n')){const i=buffer.indexOf('\n'),v=JSON.parse(buffer.slice(0,i));buffer=buffer.slice(i+1);if(pending.length)pending.shift()(v);else queue.push(v);}});
 const initial=await line(),origin=`http://${initial.address}`;
 const before=await control();
 const list=await(await fetch(origin+'/conversations')).json();
 check(list.items.length>=2,'Retained real conversations available after restart');
 const [a,b]=list.items.map(i=>i.id);
 browser=await chromium.launch({headless:true,env:{...process.env,LD_LIBRARY_PATH:process.env.BOKKIE_UI_SYSROOT?`${resolve(process.env.BOKKIE_UI_SYSROOT,'usr/lib')}:${process.env.LD_LIBRARY_PATH??''}`:(process.env.LD_LIBRARY_PATH??'')},args:['--no-sandbox','--enable-unsafe-webgpu','--enable-features=Vulkan','--use-angle=vulkan','--disable-vulkan-surface']});
 page=await browser.newPage({viewport:{width:1440,height:900}});
 const snapshot=()=>page.evaluate(()=>window.__BOKKIE_ATTENTION_HANDLE.test_snapshot());
 async function click(id){for(let i=0;i<40;i++){const s=await snapshot(),n=s.ui_snapshot.nodes.find(n=>n.id===id);if(n&&n.enabled&&n.rect.min_y>=0&&n.rect.max_y<900){await page.mouse.click((n.rect.min_x+n.rect.max_x)/2,(n.rect.min_y+n.rect.max_y)/2);return;}await page.waitForTimeout(75);}throw Error(`Control unavailable: ${id}`);}
 await page.goto(origin+'/ui/');await page.waitForFunction(()=>window.__BOKKIE_ATTENTION_HANDLE?.test_snapshot().interaction.connection==='current');
 await click('bokkie.conversation.open');await page.waitForTimeout(400);
 // The fixed desktop header is rendered by egui; the retained screenshot exposes
 // this physical pointer target, while all conversation buttons use semantic bounds.
 await page.screenshot({path:join(evidence,'before-history.png')});
 await page.mouse.click(105,137);await page.waitForTimeout(250);
 let release,heldResolve;const held=new Promise(ok=>heldResolve=ok);let intercepted=false;
 await page.route(origin+'/conversations/'+a,async route=>{if(intercepted)return route.continue();intercepted=true;await new Promise(ok=>{release=ok;heldResolve();});await route.continue();});
 await click('bokkie.conversation.history.'+a);await Promise.race([held,new Promise((_,fail)=>setTimeout(()=>fail(Error('Old conversation read was not intercepted')),5000))]);
 await click('bokkie.conversation.history.'+b);await page.waitForTimeout(500);
 release();await page.waitForTimeout(500);
 await page.screenshot({path:join(evidence,'after-delayed-history.png')});
 const state=await snapshot();
 check(state.ui_snapshot.nodes.some(n=>n.id==='bokkie.conversation.text'),'Opening B while A is outstanding leaves the conversation usable');
 const raw=JSON.stringify(state);
 check(!raw.includes('Loading conversation'),'Delayed A response does not strand history navigation');
 await click('bokkie.conversation.history.'+a);await page.waitForTimeout(500);
 check((await snapshot()).ui_snapshot.nodes.some(n=>n.id==='bokkie.conversation.text'),'Returning to A remains usable');
 const after=await control();report.model_calls=after.model_calls-before.model_calls;
 check(report.model_calls===0,'Reading and switching histories makes no model calls');
 check(JSON.stringify(before.details)===JSON.stringify(after.details),'History navigation preserves all tasks and results');
 report.passed=true;
}catch(error){report.error=String(error);process.exitCode=1;if(page)await page.screenshot({path:join(evidence,'failure.png')}).catch(()=>{});}
finally{clearTimeout(deadline);if(browser)await browser.close();if(child&&child.exitCode==null){const exited=once(child,'exit');child.stdin.end('{"stop":true}\n');await exited;}await writeFile(join(evidence,'qualification.json'),JSON.stringify(report,null,2));console.log(JSON.stringify(report));}
