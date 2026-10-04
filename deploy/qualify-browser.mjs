/** Browser acceptance for the credential-free, marked calibration stack only. */
import {chromium} from 'playwright';
import {execFileSync} from 'node:child_process';
import {mkdir, writeFile} from 'node:fs/promises';
import {resolve} from 'node:path';
const evidence=resolve(process.env.BOKKIE_DEPLOYMENT_EVIDENCE??'.ui-qualification-runtime/deployment');
await mkdir(evidence,{recursive:true});
const endpoint='http://127.0.0.1:9338';
const origin='https://bokkie-calibration.yutani.tech';
const report={source:execFileSync('git',['rev-parse','HEAD'],{encoding:'utf8'}).trim(),
  origin,model_calls:0,account:'synthetic HTTP credentials only',
  dns:'explicit diagnostic mapping; normal private DNS remains a separate acceptance check',
  responses:[],errors:[],captures:[]};
const browser=await chromium.launch({headless:true,
  env:{...process.env,LD_LIBRARY_PATH:`${resolve(process.env.BOKKIE_UI_SYSROOT??'/nvme/development/polyorama/.tools/sysroot','usr/lib')}:${process.env.LD_LIBRARY_PATH??''}`},
  args:['--no-sandbox','--enable-unsafe-webgpu','--enable-features=Vulkan','--use-angle=vulkan',
    '--disable-vulkan-surface','--remote-debugging-port=9338',
    '--host-resolver-rules=MAP bokkie-calibration.yutani.tech 192.168.50.20']});
try {
  report.browser=browser.version();
  const page=await browser.newPage({viewport:{width:1440,height:900},
    httpCredentials:{username:'calibration',password:'bokkie-synthetic-only'}});
  page.on('pageerror',error=>report.errors.push(error.message));
  page.on('response',response=>report.responses.push({path:new URL(response.url()).pathname,status:response.status()}));
  const ready=()=>page.waitForFunction(()=>window.__BOKKIE_ATTENTION_HANDLE?.test_snapshot().interaction.connection==='current',null,{timeout:30000});
  const snapshot=()=>page.evaluate(()=>JSON.parse(JSON.stringify(window.__BOKKIE_ATTENTION_HANDLE.test_snapshot(),(_,v)=>typeof v==='bigint'?v.toString():v)));
  const capture=async name=>{
    const state=await snapshot();
    await writeFile(resolve(evidence,name+'.json'),JSON.stringify(state,null,2));
    const result=JSON.parse(execFileSync('lantern',['screenshot','--endpoint',endpoint,
      '--output',resolve(evidence,name+'.png'),'--overwrite','--json'],{encoding:'utf8'}));
    if(!result.ok)throw Error('Lantern capture failed');
    report.captures.push({name,viewport:page.viewportSize(),connection:state.interaction.connection});
  };
  const response=await page.goto(origin+'/');
  if(response.status()!==200||page.url()!==origin+'/ui/')throw Error('root did not reach canonical UI URL');
  await ready();
  report.secure=await page.evaluate(()=>isSecureContext);
  if(!report.secure)throw Error('not a secure browser context');
  await capture('desktop');
  const state=await snapshot();
  const control=state.ui_snapshot.nodes.find(node=>node.id==='bokkie.conversation.open');
  if(!control?.enabled)throw Error('conversation entry point unavailable');
  const loaded=page.waitForResponse(response=>response.url()===origin+'/conversations'&&response.status()===200);
  await page.mouse.click((control.rect.min_x+control.rect.max_x)/2,(control.rect.min_y+control.rect.max_y)/2);
  await loaded;await ready();await page.waitForTimeout(300);
  if(!(await snapshot()).ui_snapshot.nodes.some(node=>node.id==='bokkie.conversation.text'))throw Error('conversation composer absent');
  await capture('conversation-desktop');
  await page.setViewportSize({width:480,height:720});await page.waitForTimeout(500);await ready();
  await capture('conversation-narrow');
  if(report.errors.length)throw Error('browser runtime errors');
  report.passed=true;
} catch(error) {
  report.passed=false;report.errors.push(String(error));process.exitCode=1;
} finally {
  await writeFile(resolve(evidence,'browser.json'),JSON.stringify(report,null,2));
  console.log(JSON.stringify({passed:report.passed,errors:report.errors,captures:report.captures}));
  await browser.close();
}
