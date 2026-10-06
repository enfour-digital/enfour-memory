// Optional browser check against the two-record deployed Enfour project fixture.
import {spawn} from 'node:child_process';
import {readFile,writeFile,mkdtemp} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {fileURLToPath} from 'node:url';
const root=fileURLToPath(new URL('..',import.meta.url)).replace(/\/$/,'');
const base=process.env.ENFOUR_URL||'http://memory.example.com:7463';
const profile=await mkdtemp(tmpdir()+'/enfour-browser-');
const browser=spawn(process.env.ENFOUR_BROWSER||'chromium',['--headless','--password-store=basic','--use-mock-keychain','--disable-gpu','--disable-background-networking','--disable-component-update','--no-first-run','--disable-sync','--no-proxy-server','--remote-debugging-address=127.0.0.1','--remote-debugging-port=0','--user-data-dir='+profile,'about:blank'],{stdio:['ignore','ignore','inherit']});
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
let socket;
try {
  let port;
  for(let i=0;i<100;i++) {try{port=(await readFile(profile+'/DevToolsActivePort','utf8')).split('\n')[0];break;}catch{await sleep(100);}}
  if(!port)throw Error('Local browser did not start');
  const target=await(await fetch('http://127.0.0.1:'+port+'/json/new?about:blank',{method:'PUT'})).json();
  socket=new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve,reject)=>{socket.onopen=resolve;socket.onerror=reject;});
  let seq=0;const pending=new Map();
  socket.onmessage=event=>{const data=JSON.parse(event.data);if(['Network.requestWillBeSent','Network.responseReceived','Network.loadingFailed'].includes(data.method))console.log(data.method,JSON.stringify({url:data.params.request?.url??data.params.response?.url,status:data.params.response?.status,error:data.params.errorText}));if(data.id){const handler=pending.get(data.id);if(handler){pending.delete(data.id);data.error?handler.reject(data.error):handler.resolve(data.result);}}};
  function rpc(method,params={}){return new Promise((resolve,reject)=>{const id=++seq;const timer=setTimeout(()=>reject(Error('Timed out: '+method)),method==='Runtime.evaluate'?120000:15000);pending.set(id,{resolve:r=>{clearTimeout(timer);resolve(r);},reject:e=>{clearTimeout(timer);reject(e);}});socket.send(JSON.stringify({id,method,params}));});}
  await rpc('Emulation.setDeviceMetricsOverride',{width:1040,height:1100,deviceScaleFactor:1,mobile:false});
  await rpc('Page.enable');
  await rpc('Network.enable');
  await rpc('Page.navigate',{url:base});
  let ready=false;
  for(let i=0;i<100;i++){
    const state=await rpc('Runtime.evaluate',{expression:"Boolean(document.getElementById('token'))",returnByValue:true});
    if(state.result.value){ready=true;break;}
    await sleep(100);
  }
  if(!ready){const state=await rpc('Runtime.evaluate',{expression:"({url:location.href,body:document.body?.innerText})",returnByValue:true});throw Error(JSON.stringify(state.result.value));}
  const token=(await readFile(root+'/state/access.token','utf8')).trim();
  let result=await rpc('Runtime.evaluate',{expression:`(async()=>{document.getElementById('token').value=${JSON.stringify(token)};await document.getElementById('token').onchange();document.getElementById('scope').value='repo:example.com/memory-demo';document.getElementById('query').value='Why did we choose SQLite?';const form=document.getElementById('search'),fetchOriginal=window.fetch;let recallRequests=0;window.fetch=(...args)=>{if(String(args[0]).startsWith('/api/recall'))recallRequests++;return fetchOriginal(...args);};const pending=form.onsubmit({preventDefault(){}});const blocked=[...form.elements].every(e=>e.disabled)&&form.getAttribute('aria-busy')==='true';await form.onsubmit({preventDefault(){}});await pending;window.fetch=fetchOriginal;return {blocked,recallRequests,released:[...form.elements].every(e=>!e.disabled)&&form.getAttribute('aria-busy')==='false',status:document.getElementById('status').textContent,scope:document.getElementById('scope').value,articles:document.querySelectorAll('article').length,title:document.querySelector('article h2')?.textContent};})()`,awaitPromise:true,returnByValue:true});
  if(result.exceptionDetails)throw Error(JSON.stringify(result.exceptionDetails));
  const value=result.result.value;
  if(value.articles!==2||value.title!=='SQLite is the single memory store'||!value.blocked||!value.released||value.recallRequests!==1)throw Error(JSON.stringify(value));
  // Remove the secret before recording the page; use a non-secret placeholder.
  await rpc('Runtime.evaluate',{expression:"document.getElementById('token').value='';document.getElementById('token').placeholder='Connected for this session';"});
  const screenshot=await rpc('Page.captureScreenshot',{format:'png'});
  await writeFile(root+'/docs/dashboard.png',Buffer.from(screenshot.data,'base64'));
  console.log(JSON.stringify({result:'PASS',...value,screenshot:'docs/dashboard.png'}));
}finally{socket?.close();browser.kill('SIGTERM');}
