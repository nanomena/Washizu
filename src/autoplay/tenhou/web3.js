/* Read-only Web3 facts and viewport CSS mouse targets, including result and
 * lobby controls. Only the shared pointer sends input. The separate UI FSM
 * owns UI permissions; observations never invoke handlers or schedule effects. */
(function createWeb3Bridge(native) {
  'use strict';

  const documentId = globalThis.crypto.randomUUID();
  const nodes = new WeakMap();
  let nextNode = 0;
  const allowedPopups = new Set(['SPLASH', 'TAIKYOKU', 'AGARI', 'RYUUKYOKU', 'OWARI', 'NINTEI']);
  const u32 = value => Number.isInteger(value) && value >= 0 && value <= 0xffffffff;
  const tile = value => Number.isInteger(value) && value >= 0 && value < 136;
  const nodeId = node => {
    if (!nodes.has(node)) nodes.set(node, ++nextNode);
    return nodes.get(node);
  };
  const connected = node => !!node && node.isConnected === true;
  const visible = node => {
    if (!connected(node) || !node.getClientRects().length) return false;
    for (let current = node; current && current.nodeType === 1; current = current.parentElement) {
      const style = getComputedStyle(current);
      if (style.display === 'none' || style.visibility === 'hidden' || Number(style.opacity) === 0) return false;
    }
    return true;
  };
  const canonical = value => {
    if (Array.isArray(value)) return '[' + value.map(canonical).join(',') + ']';
    if (value && typeof value === 'object') return '{' + Object.keys(value).sort().map(key => JSON.stringify(key) + ':' + canonical(value[key])).join(',') + '}';
    return JSON.stringify(value);
  };
  const copyPayload = value => {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
    const copy = JSON.parse(JSON.stringify(value));
    return copy && typeof copy.tag === 'string' ? copy : null;
  };
  const live = () => native.input.mode() === 1;
  const popupBlocksInput = () => {
    const popup = native.popup.read();
    // Match the client's own input gate, including a popup hidden temporarily
    // by a mouse gesture and a result being faded out.
    return connected(popup.element) && popup.element.style.display !== 'none';
  };
  const handSnapshot = () => {
    const hand = native.input.hand();
    if (!Array.isArray(hand) || !hand.length || hand.length > 14 || !hand.every(tile) || new Set(hand).size !== hand.length) return null;
    return [...hand].sort((a, b) => a - b);
  };
  const ownTarget = target => {
    if (native.input.actor() !== 0 || !native.input.isCard(target)) return false;
    const slot = native.input.targetSlot(target);
    const card = native.input.slot(slot);
    // Whose window it is, not which cards the client currently marks playable: those flags follow its UI
    // state, which a stray manual click can change. The protocol decides what may be thrown.
    return Number.isInteger(slot) && slot >= 0 && slot < 14 && card && card.target === target && card.present;
  };

  function inputSnapshot() {
    const epoch = native.epoch();
    const hand = live() ? handSnapshot() : null;
    // `blocked` names the first input gate that failed, for the host's logs only.
    const unavailable = { client: 'web3', ready: false, blocked: null, window: null, hand: hand || [], offers: [] };
    const gate = !live() ? 'not-live' : !hand ? 'no-hand' : !native.input.phaseReady() ? 'phase'
      : native.modal() ? 'modal' : popupBlocksInput() ? 'popup' : !native.clock.mounted() ? 'no-clock' : null;
    if (gate) return { dto: { ...unavailable, blocked: gate }, entries: [] };
    const target = native.clock.target();
    const source = native.menu.offers();
    if (!source || typeof source !== 'object') return { dto: { ...unavailable, blocked: 'no-menu' }, entries: [] };
    // A real timer is independent of whether a tile is currently clickable.
    // Keep its identity while waiting for native input readiness; this grants
    // no input permission and must never restart the clock on a later probe.
    const window = documentId + ':input:' + epoch;
    const discardReady = ownTarget(target);
    const responseReady = target === native.menu.responseTarget() && Object.prototype.hasOwnProperty.call(source, '2360326');
    if (!discardReady && !responseReady) return { dto: { ...unavailable, blocked: 'not-our-input', window }, entries: [] };
    const entries = [];
    for (const [key, original] of Object.entries(source)) {
      if (!/^[0-9]+$/.test(key) || !u32(Number(key))) continue;
      const payload = copyPayload(original);
      if (!payload) continue;
      entries.push({ code: Number(key), original, payload, identity: window + ':offer:' + key + ':' + canonical(payload) });
    }
    if (native.epoch() !== epoch) return { dto: { ...unavailable, blocked: 'epoch' }, entries: [] };
    return {
      dto: { client: 'web3', ready: true, blocked: null, window, hand, offers: entries.map(({ identity, payload }) => ({ identity, payload })) },
      entries, discardReady, epoch,
    };
  }

  const validRect = rect => rect && [rect.x, rect.y, rect.width, rect.height].every(Number.isFinite) && rect.width > 0 && rect.height > 0;
  const center = rect => ({ x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 });
  const contains = (rect, point) => point && Number.isFinite(point.x) && Number.isFinite(point.y)
    && point.x >= rect.x && point.y >= rect.y && point.x < rect.x + rect.width && point.y < rect.y + rect.height;
  function intersection(a, b) {
    const x = Math.max(a.x, b.x), y = Math.max(a.y, b.y);
    const right = Math.min(a.x + a.width, b.x + b.width), bottom = Math.min(a.y + a.height, b.y + b.height);
    return right > x && bottom > y ? { x, y, width: right - x, height: bottom - y } : null;
  }
  function subtract(rect, obstacle) {
    const overlap = intersection(rect, obstacle);
    if (!overlap) return [rect];
    return [
      { x: rect.x, y: rect.y, width: rect.width, height: overlap.y - rect.y },
      { x: rect.x, y: overlap.y + overlap.height, width: rect.width, height: rect.y + rect.height - overlap.y - overlap.height },
      { x: rect.x, y: overlap.y, width: overlap.x - rect.x, height: overlap.height },
      { x: overlap.x + overlap.width, y: overlap.y, width: rect.x + rect.width - overlap.x - overlap.width, height: overlap.height },
    ].filter(validRect);
  }
  function viewport() {
    const rect = { x: 0, y: 0, width: globalThis.innerWidth, height: globalThis.innerHeight };
    if (!validRect(rect)) throw new Error('Tenhou Web3 viewport unavailable');
    return rect;
  }
  function geometryContext(view) {
    const g = native.geometry.read();
    // A button the client believes held does not block targets: a stray click whose release never
    // arrived would otherwise hold the turn until its clock ran out. Manual input is the shared
    // transaction's to detect, as on every site; our own press releases the client's button state.
    if (!g || ![g.ratio, g.left, g.top, g.width, g.height, globalThis.pageXOffset, globalThis.pageYOffset].every(Number.isFinite)
      || g.ratio <= 0 || g.width <= 0 || g.height <= 0 || !visible(g.canvas) || !connected(g.menuRoot) || !connected(g.handPanel)
      || globalThis.visualViewport && globalThis.visualViewport.scale !== 1) return null;
    const canvas = g.canvas.getBoundingClientRect();
    if (!validRect(canvas)) return null;
    const cards = [];
    for (let slot = 0; slot < 14; slot++) {
      const card = native.input.slot(slot);
      if (!card) break;
      if (![card.x, card.y].every(Number.isFinite)) return null;
      cards.push({ slot, card, rect: { x: card.x / g.ratio + g.left - globalThis.pageXOffset,
        y: card.y / g.ratio + g.top - globalThis.pageYOffset, width: g.width / g.ratio, height: g.height / g.ratio } });
    }
    return { ...g, cards, viewport: view, canvas };
  }
  const blockedTags = new Set(['A', 'IMG', 'INPUT', 'SELECT', 'OPTION', 'LABEL', 'TEXTAREA']);
  function handRoute(g, point) {
    const node = document.elementFromPoint(point.x, point.y);
    if (!connected(node) || node.tagName === 'BUTTON' || blockedTags.has(node.tagName) || blockedTags.has(node.parentNode?.tagName)
      || node === g.statusCanvas || node === g.handPanel || node.parentNode === g.handPanel
      || node === g.menuRoot || node.parentNode === g.menuRoot || node.parentNode?.className === 'fixed-select') return false;
    return true;
  }
  function hitCard(g, point) {
    // This is the normal mouse branch of the verified client hit-test. It
    // intentionally includes earlier empty objects: the native loop does too.
    return g.cards.find(entry => contains(entry.rect, point)) || null;
  }
  function tileBounds(g, entry) {
    const clipped = intersection(entry.rect, g.viewport);
    if (!clipped) return null;
    let pieces = [clipped];
    for (const earlier of g.cards) {
      if (earlier.slot === entry.slot) break;
      pieces = pieces.flatMap(piece => subtract(piece, earlier.rect));
    }
    pieces.sort((a, b) => b.width * b.height - a.width * a.height);
    return pieces.find(bounds => handRoute(g, center(bounds)) && hitCard(g, center(bounds)) === entry) || null;
  }
  function controlBounds(g, control) {
    if (!control || !connected(control) || control.parentNode?.parentNode !== g.menuRoot || control.parentNode.firstChild !== control) return null;
    const outer = control.parentNode;
    if (!visible(outer)) return null;
    const rect = outer.getBoundingClientRect();
    if (!validRect(rect)) return null;
    const clipped = intersection(rect, g.viewport);
    return clipped && document.elementFromPoint(center(clipped).x, center(clipped).y) === outer ? clipped : null;
  }
  function controlTarget(g, control, identity, kind) {
    const bounds = controlBounds(g, control);
    return bounds && { control, identity: identity + ':' + kind + ':' + nodeId(control) + ':' + canonical(bounds), kind, bounds, viewport: g.viewport };
  }
  function offeredTarget(g, entry, entries) {
    // A single-choice alias is a real offered control for the exact same raw
    // payload. No red/plain or candidate-family substitution occurs here.
    for (const candidate of [entry, ...entries.filter(other => other !== entry && canonical(other.payload) === canonical(entry.payload))]) {
      const control = native.menu.control(candidate.code);
      if (control?.id !== 'm' + candidate.code) continue;
      const target = controlTarget(g, control, entry.identity + ':alias:' + candidate.code, 'offer');
      if (target) return target;
    }
    const reveal = native.menu.reveal(entry.code);
    return reveal && controlTarget(g, reveal, entry.identity, 'reveal');
  }
  function restBounds(g) {
    const clipped = intersection(g.canvas, g.viewport);
    if (!clipped) return null;
    let pieces = [clipped];
    const obstacles = g.cards.map(entry => entry.rect);
    for (const node of g.menuRoot.children) if (visible(node)) {
      const rect = node.getBoundingClientRect(); if (validRect(rect)) obstacles.push(rect);
    }
    for (const obstacle of obstacles) {
      pieces = pieces.flatMap(piece => subtract(piece, obstacle));
      if (pieces.length > 128) return null;
    }
    pieces.sort((a, b) => b.width * b.height - a.width * a.height);
    return pieces.find(rect => handRoute(g, center(rect)) && !hitCard(g, center(rect))) || null;
  }
  function geometrySnapshot() {
    const current = inputSnapshot();
    const view = viewport();
    const dto = { ...current.dto, viewport: view, clock: null, tiles: [], offers: current.dto.offers.map(offer => ({ ...offer, bounds: null, reveal: null })), nextDraw: null, rest: null };
    if (current.dto.window !== null && native.clock.mounted()) {
      const facts = native.clock.facts();
      if (facts && [facts.startedAt, facts.now, facts.fixedMs, facts.bankMs, facts.remainingMs].every(Number.isFinite)
        && facts.fixedMs >= 0 && facts.bankMs >= 0 && facts.fixedMs + facts.bankMs > 0 && facts.now >= facts.startedAt && facts.remainingMs >= 0) {
        const elapsedMs = Math.floor(facts.now - facts.startedAt), fixedMs = Math.floor(facts.fixedMs);
        const grantedBankMs = Math.floor(facts.bankMs);
        const remainingBaseMs = Math.floor(Math.max(0, facts.remainingMs - facts.bankMs));
        const remainingBankMs = Math.floor(Math.min(facts.bankMs, facts.remainingMs));
        if ([elapsedMs, fixedMs, grantedBankMs, remainingBaseMs, remainingBankMs].every(u32)) dto.clock = { elapsedMs, fixedMs, grantedBankMs, remainingBaseMs, remainingBankMs };
      }
    }
    if (!live() || !current.dto.hand.length || native.modal() || popupBlocksInput()) return { ...current, dto, geometry: null, tileEntries: [] };
    const g = geometryContext(view);
    if (!g) return { ...current, dto, geometry: null, tileEntries: [] };
    const originalHand = native.input.hand();
    const drawn = native.input.drawn();
    const tileEntries = [];
    for (let slot = 0; slot < originalHand.length; slot++) {
      const tile = originalHand[slot], entry = g.cards[slot];
      if (!entry || !entry.card.present) continue;
      const bounds = tileBounds(g, entry);
      if (!bounds) continue;
      const identity = documentId + ':tile:' + native.epoch() + ':' + canonical(current.dto.hand) + ':' + tile + ':' + slot + ':' + canonical(bounds);
      // Which tile may be thrown is the protocol's; the client's per-card flags follow its UI state.
      const raw = { identity, tile, drawn: tile === drawn, bounds };
      dto.tiles.push(raw); tileEntries.push({ ...raw, entry });
    }
    dto.offers = current.entries.map(entry => {
      const target = offeredTarget(g, entry, current.entries);
      return { identity: entry.identity, payload: entry.payload, bounds: target?.kind === 'offer' ? target.bounds : null,
        reveal: target?.kind === 'reveal' ? { identity: target.identity, bounds: target.bounds } : null };
    });
    dto.nextDraw = dto.tiles.find(entry => entry.drawn)?.bounds || null;
    dto.rest = restBounds(g);
    return { ...current, dto, geometry: g, tileEntries };
  }

  function probeTarget(request) {
    const response = (status, target = null) => ({ client: 'web3', status, target });
    if (!request || !['discard', 'offer', 'hover'].includes(request.kind)) return response('unavailable');
    if (!live()) return response('unavailable');
    const current = geometrySnapshot();
    if (!current.geometry) return response('unavailable');
    let target, cardEntry;
    if (request.kind === 'offer') {
      if (typeof request.window !== 'string' || typeof request.identity !== 'string') return response('unavailable');
      if (!current.dto.ready || current.dto.window !== request.window) return response('changed');
      const entry = current.entries.find(entry => entry.identity === request.identity);
      if (!entry || native.menu.offers()[entry.code] !== entry.original) return response('changed');
      target = offeredTarget(current.geometry, entry, current.entries);
    } else {
      if (!Array.isArray(request.hand) || !request.hand.every(tile) || !tile(request.tile)) return response('unavailable');
      const expected = [...request.hand].sort((a, b) => a - b);
      if (expected.length !== current.dto.hand.length || expected.some((value, index) => value !== current.dto.hand[index])) return response('changed');
      cardEntry = current.tileEntries.find(entry => entry.tile === request.tile);
      if (!cardEntry) return response('unavailable');
      if (request.kind === 'discard') {
        if (typeof request.window !== 'string' || typeof request.tsumogiri !== 'boolean') return response('unavailable');
        if (!current.dto.ready || current.dto.window !== request.window || !current.discardReady
          || request.tsumogiri !== cardEntry.drawn) return response('changed');
      } else if (request.identity !== cardEntry.identity) return response('changed');
      target = { identity: cardEntry.identity + ':' + request.kind + (request.kind === 'discard' ? ':' + Number(request.tsumogiri) : ''),
        kind: request.kind, bounds: cardEntry.bounds, viewport: current.dto.viewport };
    }
    if (!target) return response('unavailable');
    if ('point' in request || 'targetIdentity' in request) {
      if (request.targetIdentity !== target.identity || !contains(target.bounds, request.point)) return response('changed');
      if (cardEntry) {
        if (!handRoute(current.geometry, request.point) || hitCard(current.geometry, request.point) !== cardEntry.entry) return response('changed');
      } else if (document.elementFromPoint(request.point.x, request.point.y) !== target.control.parentNode) return response('changed');
    }
    return response('ready', { identity: target.identity, kind: target.kind, bounds: target.bounds, viewport: target.viewport });
  }

  function uiSnapshot() {
    const session = native.title.read();
    const current = native.popup.read();
    const account = typeof session.account === 'string' && /^ID[0-9A-F]{8}$/.test(session.account) ? session.account : null;
    const dto = { client: 'web3', account, disconnect: null, title: null, popup: null, lobby: null };
    const closed = native.disconnect.read();
    // Only the native dialog whose current confirmation callback came from
    // this client's own socket-close event. Unknown modals are never targets.
    if (connected(closed)) {
      const button = closed.querySelector('form > input[type="submit"]');
      const enabled = !!account && visible(closed) && visible(button) && !button.disabled;
      dto.disconnect = { identity: documentId + ':disconnect:' + nodeId(closed) + ':' + (button ? nodeId(button) : 'missing') + ':' + account + ':' + Number(enabled), enabled };
      return { dto, element: closed, button };
    }
    // Only the registered-account title control. No guest/register/ID entry.
    if (native.input.mode() === 0 && ['LOGIN', 'AUTOLOGIN'].includes(current.tag) && connected(current.element)) {
      const element = current.element;
      const button = element.querySelector('#pane0 > button[name="ok"]');
      const enabled = !!account && !current.closing && !native.modal() && visible(element) && visible(button) && !button.disabled;
      dto.title = { identity: documentId + ':title:' + nodeId(element) + ':' + (button ? nodeId(button) : 'missing') + ':' + account + ':' + Number(enabled), enabled };
      return { dto, element, button };
    }
    if (!live()) return { dto };
    const element = current.element;
    if (!connected(element)) return { dto };
    const base = documentId + ':ui:' + native.uiEpoch() + ':' + nodeId(element);
    const blocked = native.modal();
    if (current.tag !== 'LOBBY') {
      if (typeof current.tag !== 'string' || !u32(current.queued)) return { dto };
      const button = element.querySelector('button[name="ok"]');
      const enabled = allowedPopups.has(current.tag) && !current.closing && !blocked && visible(element) && visible(button) && !button.disabled;
      dto.popup = { identity: base + ':' + current.tag + ':' + current.queued + ':' + Number(current.closing) + ':' + Number(enabled) + ':' + (button ? nodeId(button) : 'missing'),
        tag: current.tag, enabled, closing: !!current.closing, queued: current.queued };
      return { dto, element, button };
    }
    const data = native.lobby.read();
    if (!u32(data.lobby) || !Array.isArray(data.selected)) return { dto };
    const identity = base + ':lobby:' + data.lobby;
    const byMode = new Map();
    for (const button of element.querySelectorAll('button[name="join"]')) {
      // Event/tournament buttons do not have a selected-rule slot. Never
      // reinterpret those buttons, move a tab, or choose a different rule.
      const match = /^join([0-5])$/.exec(button.id);
      if (!match) continue;
      const selected = data.selected[Number(match[1])];
      if (typeof selected !== 'string' || !/^[0-9]+$/.test(selected) || !u32(Number(selected))) continue;
      const mode = Number(selected);
      // Hidden lobby panes cannot supply a mouse target. Prefer a genuinely
      // visible alias of the exact selected rule; never invoke a hidden control.
      const enabled = !current.closing && !blocked && visible(element) && visible(button) && data.loaded && !data.reservationRequested && !button.disabled;
      const offer = { identity: identity + ':join:' + nodeId(button) + ':' + mode, mode, enabled: !!enabled };
      // A mouse press lets the official handler read selected[slot] and reserve
      // that exact lobby/mode. Main rows and shortcut panes may select the
      // same mode; normalize those aliases before the FSM sees a choice.
      // Prefer an enabled control, then the lowest native rule slot. Keep
      // the actual button identity so replacements revoke pending effects.
      const slot = Number(match[1]);
      const previous = byMode.get(mode);
      if (!previous || enabled && !previous.offer.enabled
        || !!enabled === previous.offer.enabled && slot < previous.slot) {
        byMode.set(mode, { offer, button, slot });
      }
    }
    const entries = [...byMode.values()].sort((a, b) => a.slot - b.slot);
    const offers = entries.map(entry => entry.offer);
    dto.lobby = { identity, lobby: data.lobby, offers, reservationRequested: !!data.reservationRequested };
    return { dto, element, entries };
  }

  function probeUi(request) {
    const response = (status, target = null) => ({ client: 'web3', status, target });
    if (!request || !['popup', 'join', 'resume', 'disconnect'].includes(request.kind) || typeof request.identity !== 'string') return response('unavailable');
    const current = uiSnapshot();
    let button;
    if (request.kind === 'disconnect') {
      if (!current.dto.disconnect || current.dto.disconnect.identity !== request.identity || !current.dto.disconnect.enabled) return response('changed');
      button = current.button;
    } else if (request.kind === 'resume') {
      if (!current.dto.title || current.dto.title.identity !== request.identity || !current.dto.title.enabled) return response('changed');
      button = current.button;
    } else if (request.kind === 'popup') {
      if (!current.dto.popup || current.dto.popup.identity !== request.identity || !current.dto.popup.enabled) return response('changed');
      button = current.button;
    } else {
      const entry = current.entries?.find(entry => entry.offer.identity === request.identity);
      if (!entry || !entry.offer.enabled) return response('changed');
      button = entry.button;
    }
    if (!visible(button) || button.disabled || !current.element.contains(button)) return response('changed');
    const view = viewport(), raw = button.getBoundingClientRect();
    if (!validRect(raw)) return response('unavailable');
    const bounds = intersection(raw, view);
    if (!bounds) return response('unavailable');
    const identity = request.identity + ':mouse:' + nodeId(button) + ':' + canonical({ bounds, viewport: view });
    const point = 'point' in request ? request.point : center(bounds);
    if ('point' in request || 'targetIdentity' in request) {
      if (request.targetIdentity !== identity || !contains(bounds, point)) return response('changed');
    }
    const hit = document.elementFromPoint(point.x, point.y);
    if (hit !== button && !button.contains(hit)) return response('changed');
    return response('ready', { identity, bounds, viewport: view });
  }

  return Object.freeze({
    client: 'web3',
    readUi() { return uiSnapshot().dto; },
    probeUi,
    readInput() { return geometrySnapshot().dto; },
    probeTarget,
  });
})
