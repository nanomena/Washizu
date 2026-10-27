import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';
import { test } from 'node:test';

const source = readFileSync(new URL('./web3.js', import.meta.url), 'utf8');
let documentNo = 0;
class Element {
  constructor(tag = 'DIV', rect = { x: 0, y: 0, width: 800, height: 600 }) {
    Object.assign(this, { tagName: tag, nodeType: 1, isConnected: true, style: { display: '', opacity: '1', visibility: 'visible' },
      parentNode: null, parentElement: null, firstChild: null, children: [], rect, id: '', name: '', disabled: false });
  }
  getClientRects() { return this.style.display === 'none' ? [] : [this.rect]; }
  getBoundingClientRect() { return this.rect; }
  append(child) { this.children.push(child); child.parentNode = child.parentElement = this; this.firstChild = this.children[0]; }
  contains(child) { return this.children.includes(child); }
  querySelector() { return this.children.find(child => child.name === 'ok') || null; }
  querySelectorAll() { return this.children.filter(child => child.name === 'join'); }
}
class Card {}
const contains = (r, p) => p.x >= r.x && p.x < r.x + r.width && p.y >= r.y && p.y < r.y + r.height;
const middle = r => ({ x: r.x + r.width / 2, y: r.y + r.height / 2 });
const plain = value => JSON.parse(JSON.stringify(value));
function fixture() {
  const canvas = new Element('CANVAS'), menuRoot = new Element(), handPanel = new Element();
  const state = { epoch: 1, uiEpoch: 1, live: 1, actor: 0, hand: [0, 4, 8, 12, 16, 17, 20, 24, 28, 32, 36, 40, 44, 48], drawn: 48,
    riichi: false, ready: true, mounted: true, pressed: false, modal: false, popup: { element: null }, controls: {}, offers: {},
    selected: ['1', '9'], loaded: true, reservationRequested: false,
    ratio: 2, left: 20, top: 30, width: 80, height: 120, scrollX: 0, scrollY: 0, overlay: null, effects: [],
    clockFacts: { startedAt: 100, now: 250, fixedMs: 5000, bankMs: 7000, remainingMs: 11850 } };
  state.cards = state.hand.map((_, i) => Object.assign(new Card(), { slot: i, present: true, legal: true, blocked: false, x: i * 82, y: 800 }));
  state.target = state.cards[13];
  const native = {
    epoch: () => state.epoch, uiEpoch: () => state.uiEpoch, modal: () => state.modal,
    input: { mode: () => state.live, phaseReady: () => state.ready, hand: () => state.hand, actor: () => state.actor,
      drawn: () => state.drawn, riichi: () => state.riichi, isCard: value => value instanceof Card, targetSlot: value => value.slot,
      slot: i => state.cards[i] && { ...state.cards[i], target: state.cards[i] } },
    clock: { mounted: () => state.mounted, target: () => state.target,
      facts: () => state.clockFacts },
    menu: { offers: () => state.offers, responseTarget: () => state.response, control: code => state.controls[code],
      reveal: code => state.controls[code & 8192 ? 3670533 : code & 16384 ? 3671045 : -1] },
    geometry: { read: () => ({ ...state, canvas, menuRoot, handPanel }) },
    popup: { read: () => state.popup, ok: () => state.effects.push('popup') },
    lobby: { read: () => ({ lobby: 0, selected: state.selected, reservationRequested: state.reservationRequested, loaded: state.loaded }),
      join: button => { state.effects.push(button.id); state.reservationRequested = true; state.uiEpoch++; } },
  };
  const context = vm.createContext({ native, crypto: { randomUUID: () => 'doc-' + ++documentNo }, innerWidth: 800, innerHeight: 600,
    pageXOffset: 0, pageYOffset: 0, getComputedStyle: node => node.style,
    document: { elementFromPoint(x, y) {
      if (state.overlay && contains(state.overlay.rect, { x, y })) return state.overlay;
      for (const outer of menuRoot.children) if (outer.style.display !== 'none' && outer.style.visibility !== 'hidden' && contains(outer.rect, { x, y })) return outer;
      return canvas;
    } } });
  const api = vm.runInContext('(' + source + ')(native)', context);
  function control(code, rect, shown = true) {
    const outer = new Element('DIV', rect), inner = new Element(); inner.id = 'm' + code;
    outer.style.display = shown ? '' : 'none'; outer.append(inner); menuRoot.append(outer); state.controls[code] = inner; return outer;
  }
  const probeDiscard = (tile = 16, tsumogiri = false) => {
    const snap = api.readInput(); const request = { kind: 'discard', window: snap.window, hand: snap.hand, tile, tsumogiri };
    return { request, probe: api.probeTarget(request) };
  };
  return { state, api, context, native, canvas, menuRoot, handPanel, control, probeDiscard };
}

test('playing API is read-only, exact physical red/plain geometry uses native scale and offsets', () => {
  const { state, api, probeDiscard } = fixture();
  assert.equal(api.discard, undefined); assert.equal(api.choose, undefined);
  const before = { epoch: state.epoch, hand: [...state.hand] };
  const red = probeDiscard(16), normal = probeDiscard(17);
  assert.equal(red.probe.status, 'ready'); assert.equal(normal.probe.status, 'ready');
  assert.equal(red.probe.target.bounds.x, 184); assert.equal(red.probe.target.bounds.y, 430);
  assert.equal(normal.probe.target.bounds.x, 225); assert.equal(red.probe.target.bounds.width, 40);
  assert.equal(api.probeTarget({ ...red.request, targetIdentity: red.probe.target.identity, point: middle(red.probe.target.bounds) }).status, 'ready');
  assert.deepEqual(state.effects, []); assert.deepEqual({ epoch: state.epoch, hand: state.hand }, before);
});
test('tsumogiri and a riichi-locked discard cannot be changed to another physical copy', () => {
  const f = fixture();
  assert.equal(f.probeDiscard(48, true).probe.status, 'ready');
  assert.equal(f.probeDiscard(48, false).probe.status, 'changed');
  assert.equal(f.probeDiscard(16, true).probe.status, 'changed');
  f.state.riichi = true;
  assert.equal(f.probeDiscard(16, false).probe.status, 'changed');
  assert.equal(f.probeDiscard(48, true).probe.status, 'ready');
});
test('hidden, occluded, overlapping and foreign DOM routes do not become mouse targets', () => {
  for (const change of [f => { f.state.overlay = new Element('BUTTON'); }, f => { f.state.cards[3].x = f.state.cards[4].x; },
    f => { f.canvas.style.display = 'none'; }, f => { f.state.pressed = true; },
    f => { f.context.document.elementFromPoint = () => f.handPanel; }]) {
    const f = fixture(); change(f); assert.notEqual(f.probeDiscard().probe.status, 'ready'); assert.deepEqual(f.state.effects, []);
  }
});
test('final point check rejects a changed viewport, layout, native window, hand or actual hit', () => {
  for (const change of [f => { f.state.left += 4; }, f => { f.state.epoch++; }, f => { f.state.hand.pop(); },
    f => { f.context.pageYOffset = 10; }, f => { f.state.overlay = new Element('BUTTON'); }]) {
    const f = fixture(), first = f.probeDiscard(); change(f);
    assert.notEqual(f.api.probeTarget({ ...first.request, targetIdentity: first.probe.target.identity, point: middle(first.probe.target.bounds) }).status, 'ready');
  }
});
test('nonlegal visible cards provide hover facts without input permission', () => {
  const f = fixture(); f.state.ready = false; f.state.mounted = false; f.state.cards[4].legal = false;
  const snapshot = f.api.readInput(); const tile = snapshot.tiles.find(tile => tile.tile === 16);
  assert.equal(snapshot.ready, false); assert.equal(tile.legal, false);
  assert.equal(f.api.probeTarget({ kind: 'hover', hand: snapshot.hand, tile: 16, identity: tile.identity }).status, 'ready');
  assert.notEqual(f.api.probeTarget({ kind: 'discard', window: 'old', hand: snapshot.hand, tile: 16, tsumogiri: false }).status, 'ready');
});
test('offer payloads stay exact and visible aliases only match the same raw action', () => {
  const f = fixture(); f.state.offers = { 401412: { tag: 'N', type: 5, hai: 16 } };
  f.state.offers[2098693] = f.state.offers[401412];
  f.control(401412, { x: 10, y: 30, width: 80, height: 40 }, false);
  f.control(2098693, { x: 110, y: 30, width: 80, height: 40 });
  const snapshot = f.api.readInput(); const offer = snapshot.offers.find(offer => offer.identity.includes(':401412:'));
  assert.equal(offer.bounds.x, 110); assert.deepEqual(plain(offer.payload), { tag: 'N', type: 5, hai: 16 });
  const probe = f.api.probeTarget({ kind: 'offer', window: snapshot.window, identity: offer.identity });
  assert.equal(probe.status, 'ready'); assert.equal(probe.target.kind, 'offer'); assert.deepEqual(f.state.effects, []);
});
test('hidden multi-choice offer reports a reveal target; only observed geometry changes the next target', () => {
  const f = fixture(); f.state.offers = { 409606: { tag: 'N', type: 1, hai0: 16, hai1: 17 } };
  const hidden = f.control(409606, { x: 10, y: 30, width: 80, height: 40 }, false);
  const reveal = f.control(3671045, { x: 110, y: 30, width: 80, height: 40 });
  const first = f.api.readInput(); const request = { kind: 'offer', window: first.window, identity: first.offers[0].identity };
  const probe = f.api.probeTarget(request); assert.equal(probe.target.kind, 'reveal'); assert.equal(first.offers[0].bounds, null);
  assert.equal(first.offers[0].reveal.bounds.x, 110); assert.deepEqual(f.state.effects, []);
  reveal.style.display = 'none'; hidden.style.display = '';
  assert.equal(f.api.probeTarget(request).target.kind, 'offer');
  assert.equal(f.api.probeTarget({ ...request, targetIdentity: probe.target.identity, point: middle(probe.target.bounds) }).status, 'changed');
});
test('non-live clients never expose private tile targets', () => {
  for (const live of [0, 2, 3, 4]) {
    const f = fixture(); f.state.live = live; const snapshot = f.api.readInput();
    assert.deepEqual(plain(snapshot.hand), []); assert.deepEqual(plain(snapshot.tiles), []);
    assert.equal(f.api.probeTarget({ kind: 'discard' }).status, 'unavailable');
  }
});
test('clock origin and actual granted bank survive repeated reads and bank consumption', () => {
  const f = fixture();
  assert.deepEqual(plain(f.api.readInput().clock), { elapsedMs: 150, fixedMs: 5000, grantedBankMs: 7000, remainingBaseMs: 4850, remainingBankMs: 7000 });
  f.state.clockFacts.now = 6100; f.state.clockFacts.remainingMs = 6000;
  assert.deepEqual(plain(f.api.readInput().clock), { elapsedMs: 6000, fixedMs: 5000, grantedBankMs: 7000, remainingBaseMs: 0, remainingBankMs: 6000 });
  assert.deepEqual(plain(f.api.readInput().clock), plain(f.api.readInput().clock));
  f.state.ready = false; assert.equal(f.api.readInput().clock, null);
});

function lobby(f, selected, slots) {
  const element = new Element();
  const buttons = slots.map(slot => { const button = new Element('BUTTON'); button.name = 'join'; button.id = 'join' + slot; element.append(button); return button; });
  f.state.selected = selected;
  f.state.popup = { element, tag: 'LOBBY', closing: false, queued: 0 };
  return buttons;
}
test('live lobby shortcut aliases reserve one exact rule through a canonical real control', () => {
  const f = fixture();
  // Actual lobby 0 has three enabled aliases for mode 1, including shortcuts.
  lobby(f, ['1', '0', '0', '1', '1', '9'], [4, 3, 0, 1, 2, 5]);
  const first = f.api.readUi();
  assert.deepEqual(plain(first.lobby.offers.map(offer => offer.mode)), [1, 0, 9]);
  assert.equal(first.lobby.offers.filter(offer => offer.mode === 1).length, 1);
  assert.equal(f.api.pressUi('join', first.lobby.offers[0].identity), 'sent');
  assert.deepEqual(f.state.effects, ['join0']);
  assert.equal(f.api.readUi().lobby.reservationRequested, true);
  assert.equal(f.api.pressUi('join', first.lobby.offers[0].identity), 'changed');
});
test('a disabled or replaced lobby alias revokes its old permission without changing rules', () => {
  const f = fixture(), buttons = lobby(f, ['1', '9', '33', '1'], [0, 1, 3]);
  const first = f.api.readUi().lobby.offers.find(offer => offer.mode === 1);
  buttons[0].disabled = true;
  const next = f.api.readUi().lobby.offers.find(offer => offer.mode === 1);
  assert.equal(next.enabled, true); assert.notEqual(next.identity, first.identity);
  assert.equal(f.api.pressUi('join', first.identity), 'changed');
  f.state.selected[3] = '33';
  assert.equal(f.api.pressUi('join', next.identity), 'changed');
  assert.deepEqual(f.state.effects, []);
  const unavailable = f.api.readUi().lobby.offers.find(offer => offer.mode === 1);
  assert.equal(unavailable.enabled, false);
});

test('CDP attach recognizes the read-only playing contract without forcing a reload', () => {
  const capture = readFileSync(new URL('../../capture/cdp.rs', import.meta.url), 'utf8');
  const expression = JSON.parse(capture.match(/let expr = ("[^\n]+window\.__akagiTenhou[^\n]+");/)[1]);
  const api = fixture().api;
  assert.equal(vm.runInNewContext(expression, { window: { __akagiTenhou: api }, location: { host: 'tenhou.net' } }), true);
  assert.equal(vm.runInNewContext(expression, { window: { __akagiTenhou: { ...api, probeTarget: undefined } }, location: { host: 'tenhou.net' } }), false);
  assert.equal(vm.runInNewContext(expression, { window: {}, location: { host: 'game.maj-soul.com' } }), true);
});
