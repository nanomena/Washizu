import assert from 'node:assert/strict'
import fs from 'node:fs'
import vm from 'node:vm'
import test from 'node:test'

const rust = fs.readFileSync(new URL('./mod.rs', import.meta.url), 'utf8')
const source = name => rust.match(new RegExp(`const ${name}: &str = r#"([\\s\\S]*?)"#;`))?.[1]
const read = source('READ_VIEW')
const validate = source('VALIDATE_POINT')
assert.ok(read && validate)

function fixture() {
  const canvas = { getBoundingClientRect: () => ({ x: 100, y: 50, width: 1280, height: 720 }) }
  const button = { name: 'btn_lizhi', visible: true, alpha: 1, width: 180, height: 80,
    localToGlobal: p => ({ x: p.x + 1200, y: p.y + 700 }),
    hitTestPoint: (x, y) => x >= 1200 && x <= 1380 && y >= 700 && y <= 780,
    contains: target => target === button }
  const row = { visible: true, alpha: 1, numChildren: 1, getChildAt: () => button,
    getChildByName: name => name === button.name ? button : null }
  button.parent = row
  const red = { val: { toString: () => '0m', baida: false }, valid: true }
  const draw = { val: { toString: () => '5m', baida: false }, valid: true }
  const me = { hand: [red, draw], last_tile: draw, can_discard: true,
    needCheckMouse: () => true, getMouseInfo: () => ({ pai: draw }),
    onMouseDown: () => assert.fail('observation cannot send input'),
    doChoosePaiAction: () => assert.fail('observation cannot choose a tile') }
  const Laya = { stage: { width: 1920, height: 1080 },
    Point: class { constructor(x, y) { this.x = x; this.y = y } },
    MouseManager: { instance: { mouseX: 950, mouseY: 540, _target: button } } }
  const context = vm.createContext({ Laya,
    document: { querySelector: () => canvas, elementFromPoint: () => canvas },
    window: { Laya, view: { DesktopMgr: { Inst: { gameing: true, mainrole: me } } },
      uiscript: { UI_LiQiZiMo: { Inst: { enable: true, container_btns: row } } } } })
  return { context, button, me, Laya, canvas, red, draw }
}

test('read-only observation reports actual visible controls in viewport CSS pixels', () => {
  const f = fixture()
  const view = vm.runInContext(read, f.context)
  assert.deepEqual(JSON.parse(JSON.stringify(view.tiles)), [{ tile: '0m', drawn: false }, { tile: '5m', drawn: true }])
  assert.deepEqual(JSON.parse(JSON.stringify(view.controls[0].bounds)), { x: 900, y: 516.6666666666667, width: 120, height: 53.333333333333336 })
  f.button.visible = false
  assert.equal(vm.runInContext(read, f.context).controls.length, 0)
})

test('actual final point requires the same native tile, full hand, cursor and unobscured canvas', () => {
  const f = fixture()
  const request = { point: [950, 540], slot: 1, control: 'tile', hand: ['0m:false', '5m:true'] }
  const check = () => vm.runInContext(`(${validate})(${JSON.stringify(request)})`, f.context)
  assert.equal(check(), true)
  request.slot = 0
  assert.equal(check(), false)
  request.slot = 1
  f.context.document.elementFromPoint = () => ({ overlay: true })
  assert.equal(check(), false)
  f.context.document.elementFromPoint = () => f.canvas
  f.Laya.MouseManager.instance.mouseX = 100
  assert.equal(check(), false)
  f.Laya.MouseManager.instance.mouseX = 950
  f.red.val.toString = () => '5m'
  assert.equal(check(), false)
})

test('button validation requires the native hit area and actual mouse target', () => {
  const f = fixture()
  const request = { point: [950, 540], slot: null, control: 'btn_lizhi', hand: ['0m:false', '5m:true'] }
  const check = () => vm.runInContext(`(${validate})(${JSON.stringify(request)})`, f.context)
  assert.equal(check(), true)
  f.Laya.MouseManager.instance._target = { different: true }
  assert.equal(check(), false)
  f.Laya.MouseManager.instance._target = f.button
  f.button.hitTestPoint = () => false
  assert.equal(check(), false)
})
