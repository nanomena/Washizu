// Isolated real Chromium DOM and trusted CDP mouse events. No login, game,
// model, external network or system-cursor movement is involved.
import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { app, BrowserWindow } from 'electron'

const home = fs.mkdtempSync(path.join(os.tmpdir(), 'washizu-tenhou-ui-mouse-'))
app.setPath('userData', home)
const source = fs.readFileSync(new URL('../../src/autoplay/tenhou/web3.js', import.meta.url), 'utf8')
/** @type {BrowserWindow | undefined} */
let window

async function main() {
  try {
    await app.whenReady()
    const win = window = new BrowserWindow({ show: false, width: 800, height: 600,
      webPreferences: { sandbox: true, contextIsolation: true, backgroundThrottling: false } })
    win.webContents.on('console-message', details => { if (details.level === 'error') console.error(details.message) })
    const fixture = path.join(home, 'fixture.html')
    fs.writeFileSync(fixture, '<main id="panel"><button name="join" id="join0">Reserve</button><button name="ok">OK</button></main><style>button{position:absolute;top:80px;width:100px;height:40px}#join0{left:80px}button[name=ok]{left:240px}</style>')
    await win.loadFile(fixture)
    await win.webContents.executeJavaScript(`{
      const panel=document.querySelector('#panel'),join=document.querySelector('#join0'),ok=document.querySelector('[name=ok]');
      const state=window.fixture={tag:'LOBBY',epoch:1,reserved:false,effects:[],mouse:[],mode:1,title:null,disconnect:null};
      for(const type of ['mousedown','mouseup','click'])document.addEventListener(type,event=>state.mouse.push({type,trusted:event.isTrusted}));
      join.addEventListener('click',event=>{state.effects.push({tag:'JOIN',trusted:event.isTrusted});state.reserved=true;state.epoch++;});
      ok.addEventListener('click',event=>{state.effects.push({tag:state.tag,trusted:event.isTrusted});state.epoch++;});
      window.api=(${source})({input:{mode:()=>state.mode},title:{read:()=>({account:'ID1234ABCD'})},uiEpoch:()=>state.epoch,modal:()=>false,
        disconnect:{read:()=>state.disconnect},
        popup:{read:()=>({element:state.title||panel,tag:state.tag,queued:0,closing:false})},
        lobby:{read:()=>({lobby:0,selected:['1'],loaded:true,reservationRequested:state.reserved})}}); true;
    }`)
    win.webContents.debugger.attach('1.3')
    const read = (/** @type {string} */ expression) => win.webContents.executeJavaScript(expression)
    const target = async (/** @type {'join'|'popup'|'resume'|'disconnect'} */ kind) => {
      const value = await read(`{const ui=api.readUi();const identity=${kind === 'join' ? 'ui.lobby.offers[0].identity' : kind === 'resume' ? 'ui.title.identity' : kind === 'disconnect' ? 'ui.disconnect.identity' : 'ui.popup.identity'};({kind:${JSON.stringify(kind)},identity,probe:api.probeUi({kind:${JSON.stringify(kind)},identity})})}`)
      assert.equal(value.probe.status, 'ready')
      return value
    }
    const click = async (/** @type {any} */ value) => {
      const { bounds, identity } = value.probe.target
      const point = { x: bounds.x + bounds.width / 2, y: bounds.y + bounds.height / 2 }
      const check = await read(`api.probeUi(${JSON.stringify({kind:value.kind,identity:value.identity,targetIdentity:identity,point})}).status`)
      assert.equal(check, 'ready')
      await win.webContents.debugger.sendCommand('Input.dispatchMouseEvent', {type:'mouseMoved',...point})
      await win.webContents.debugger.sendCommand('Input.dispatchMouseEvent', {type:'mousePressed',...point,button:'left',clickCount:1})
      await win.webContents.debugger.sendCommand('Input.dispatchMouseEvent', {type:'mouseReleased',...point,button:'left',clickCount:1})
    }
    assert.equal(await read('typeof api.pressUi'), 'undefined')
    const join = await target('join')
    assert.deepEqual(await read('fixture.effects'), [])
    await read(`{const cover=document.createElement('div');cover.id='cover';cover.style='position:absolute;inset:0;z-index:10';document.body.append(cover);}`)
    assert.equal(await read(`api.probeUi(${JSON.stringify({kind:join.kind,identity:join.identity})}).status`), 'changed')
    assert.deepEqual(await read('fixture.effects'), [])
    await read(`document.querySelector('#cover').remove()`)
    // Removing an obstacle may change viewport/layout. Acquire a fresh target,
    // as the FSM does after a refused effect; never reuse the stale witness.
    await click(await target('join'))
    assert.equal(await read('api.readUi().lobby.reservationRequested'), true)
    for (const tag of ['SPLASH', 'TAIKYOKU', 'AGARI', 'RYUUKYOKU', 'OWARI', 'NINTEI']) {
      await read(`fixture.tag=${JSON.stringify(tag)};fixture.epoch++`)
      await click(await target('popup'))
    }
    await read(`{
      document.querySelector('#panel').style.display='none';
      const modal=document.createElement('div');modal.innerHTML='<form><input type="button" value="CANCEL"><input type="submit" value="OK"></form>';
      modal.querySelector('form').style='position:absolute;left:80px;top:80px';
      document.body.append(modal);fixture.disconnect=modal;
      modal.querySelector('form').addEventListener('submit',event=>event.preventDefault());
      modal.querySelector('[type=submit]').addEventListener('click',event=>{fixture.effects.push({tag:'DISCONNECT',trusted:event.isTrusted});fixture.disconnect=null;modal.remove();});
    }`)
    await click(await target('disconnect'))
    assert.equal(await read('api.readUi().disconnect'), null)
    await read(`{
      document.querySelector('#panel').remove();fixture.mode=0;fixture.tag='LOGIN';
      const title=document.createElement('main');title.innerHTML='<div id="pane1"><button name="ok">Guest</button></div><div id="pane0"><button name="ok" style="left:80px">OK</button></div>';
      document.body.append(title);fixture.title=title;
      title.querySelector('#pane0 > [name=ok]').addEventListener('click',event=>{fixture.effects.push({tag:'RESUME',trusted:event.isTrusted});fixture.mode=1;title.remove();});
    }`)
    await click(await target('resume'))
    assert.equal(await read('api.readUi().title'), null)
    const effects = await read('fixture.effects'), mouse = await read('fixture.mouse')
    assert.deepEqual(effects.map((/** @type {any} */ event) => event.tag), ['JOIN','SPLASH','TAIKYOKU','AGARI','RYUUKYOKU','OWARI','NINTEI','DISCONNECT','RESUME'])
    assert.ok(effects.every((/** @type {any} */ event) => event.trusted))
    assert.equal(mouse.length, 27)
    assert.ok(mouse.every((/** @type {any} */ event) => event.trusted))
    console.log('ok read-only Tenhou UI probes; nine trusted Chromium mouse presses including welcome, disconnect confirmation and title resume')
    win.webContents.debugger.detach()
  } catch (error) { console.error(error); process.exitCode = 1 }
  finally { window?.destroy(); app.exit(Number(process.exitCode) || 0) }
}
void main()
