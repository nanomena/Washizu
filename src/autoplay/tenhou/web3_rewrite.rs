//! Instrument only the Web3 client's verified native closures.
//!
//! Protocol tags, payload shapes and DOM names anchor every recovered name.
//! A changed main client is a contract error; helper scripts are NotClient.
//! No downloaded client bytes, compressed names or 4K path are bundled here.

use std::collections::BTreeMap;
use std::fmt;

const PRODUCER: &str = include_str!("web3.js");
const MARKER: &str = "/*washizu-web3-bridge*/";
const EPOCH: &str = "__washizuWeb3Epoch";
const UI_EPOCH: &str = "__washizuWeb3UiEpoch";
const DISCONNECT_METADATA: &str = "BYE:function(__disconnect){__state.connected&=~(1<<~~__disconnect.who);__status.__statusRefresh();return 1}";
const DISCONNECT_DIALOG: &str = r#"close:function(){this==__disconnectSocket&&(__disconnectQueue=[],__disconnectModal.__showModal("CONNECTION CLOSED<br>"+__disconnectI18n(__disconnectStrings.__disconnectText),3,function(){location.reload()}))}"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewriteError {
    NotClient,
    Contract(String),
}

impl fmt::Display for RewriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotClient => write!(f, "not the Tenhou Web3 main client"),
            Self::Contract(reason) => write!(f, "Tenhou Web3 contract: {reason}"),
        }
    }
}

impl std::error::Error for RewriteError {}

fn contract(reason: impl Into<String>) -> RewriteError {
    RewriteError::Contract(reason.into())
}

#[derive(Debug)]
struct Token<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    identifier: bool,
}

fn ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

// This scanner preserves source offsets, skipping strings/comments/regexes
// as units so braces in a regular expression cannot become scope boundaries.
fn tokens(src: &str) -> Result<Vec<Token<'_>>, RewriteError> {
    let bytes = src.as_bytes();
    let mut out: Vec<Token<'_>> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if bytes[i..].starts_with(b"//") {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            let Some(length) = src[i + 2..].find("*/") else {
                return Err(contract("unclosed comment"));
            };
            i += length + 4;
            continue;
        }
        let start = i;
        let identifier = bytes[i].is_ascii_alphabetic() || matches!(bytes[i], b'_' | b'$');
        if ident(bytes[i]) {
            i += 1;
            while i < bytes.len() && ident(bytes[i]) {
                i += 1;
            }
        } else if matches!(bytes[i], b'\'' | b'"') {
            let quote = bytes[i];
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            if i >= bytes.len() {
                return Err(contract("unclosed string"));
            }
            i += 1;
        } else if bytes[i] == b'`' {
            return Err(contract(
                "template literals need a reviewed scanner contract",
            ));
        } else if bytes[i] == b'/'
            && out.last().is_none_or(|last| {
                matches!(
                    last.text,
                    "(" | "["
                        | "{"
                        | "="
                        | ":"
                        | ","
                        | ";"
                        | "!"
                        | "?"
                        | "&"
                        | "|"
                        | "~"
                        | "return"
                        | "case"
                        | "throw"
                )
            })
        {
            i += 1;
            let mut bracket = false;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'[' => {
                        bracket = true;
                        i += 1;
                    }
                    b']' => {
                        bracket = false;
                        i += 1;
                    }
                    b'/' if !bracket => break,
                    _ => i += 1,
                }
            }
            if i >= bytes.len() {
                return Err(contract("unclosed regex"));
            }
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
        } else {
            i += src[i..]
                .chars()
                .next()
                .ok_or_else(|| contract("invalid source offset"))?
                .len_utf8();
        }
        out.push(Token {
            text: &src[start..i],
            start,
            end: i,
            identifier,
        });
    }
    Ok(out)
}

struct Source<'a> {
    code: &'a str,
    tokens: Vec<Token<'a>>,
    pairs: BTreeMap<usize, usize>,
    names: BTreeMap<String, String>,
}

#[derive(Clone, Copy)]
struct Scope {
    open: usize,
    close: usize,
}

#[derive(Clone)]
struct Method {
    name: String,
    body: Scope,
}

impl<'a> Source<'a> {
    fn new(code: &'a str) -> Result<Self, RewriteError> {
        let tokens = tokens(code)?;
        let mut pairs = BTreeMap::new();
        let mut stack = Vec::new();
        for (i, token) in tokens.iter().enumerate() {
            match token.text {
                "(" | "[" | "{" => stack.push(i),
                ")" | "]" | "}" => {
                    let open = stack
                        .pop()
                        .ok_or_else(|| contract("unbalanced delimiters"))?;
                    let expected = match tokens[open].text {
                        "(" => ")",
                        "[" => "]",
                        _ => "}",
                    };
                    if token.text != expected {
                        return Err(contract("mismatched delimiters"));
                    }
                    pairs.insert(open, i);
                }
                _ => {}
            }
        }
        if !stack.is_empty() {
            return Err(contract("unclosed delimiters"));
        }
        Ok(Self {
            code,
            tokens,
            pairs,
            names: BTreeMap::new(),
        })
    }

    fn unique(&mut self, label: &str, pattern: &str) -> Result<usize, RewriteError> {
        let pattern = tokens(pattern)?;
        let mut matches = Vec::new();
        if pattern.len() > self.tokens.len() {
            return Err(contract(format!(
                "{label}: source is shorter than the required shape"
            )));
        }
        for start in 0..=self.tokens.len().saturating_sub(pattern.len()) {
            // Reject the token shape without allocating captures. A main
            // script has tens of thousands of possible starts; cloning all
            // previously bound names at each one would block interception.
            if !pattern
                .iter()
                .zip(&self.tokens[start..])
                .all(|(expected, actual)| {
                    if expected.identifier && expected.text.starts_with("__") {
                        actual.identifier
                            && self
                                .names
                                .get(expected.text)
                                .is_none_or(|value| value == actual.text)
                    } else {
                        expected.text == actual.text
                    }
                })
            {
                continue;
            }
            let mut names = self.names.clone();
            let mut valid = true;
            for (expected, actual) in pattern.iter().zip(&self.tokens[start..]) {
                if expected.identifier && expected.text.starts_with("__") {
                    if !actual.identifier {
                        valid = false;
                        break;
                    }
                    if let Some(value) = names.get(expected.text) {
                        if value != actual.text {
                            valid = false;
                            break;
                        }
                    } else {
                        names.insert(expected.text.to_owned(), actual.text.to_owned());
                    }
                } else if expected.text != actual.text {
                    valid = false;
                    break;
                }
            }
            if valid {
                matches.push((start, names));
            }
        }
        if matches.len() != 1 {
            return Err(contract(format!(
                "{label}: expected one shape, found {}",
                matches.len()
            )));
        }
        let (start, names) = matches.pop().expect("one shape");
        self.names = names;
        Ok(start)
    }

    fn name(&self, key: &str) -> &str {
        &self.names[key]
    }

    fn pair(&self, open: usize) -> Result<usize, RewriteError> {
        self.pairs
            .get(&open)
            .copied()
            .ok_or_else(|| contract("missing scope boundary"))
    }

    fn enclosing_function(&self, at: usize) -> Result<Scope, RewriteError> {
        let mut found = None;
        for i in 0..at {
            if self.tokens[i].text != "function" {
                continue;
            }
            let args = i + if self.tokens.get(i + 1).is_some_and(|t| t.identifier) {
                2
            } else {
                1
            };
            if self.tokens.get(args).is_none_or(|t| t.text != "(") {
                continue;
            }
            let open = self.pair(args)? + 1;
            if self.tokens.get(open).is_some_and(|t| t.text == "{") {
                let close = self.pair(open)?;
                if open < at && at < close {
                    found = Some(Scope { open, close });
                }
            }
        }
        found.ok_or_else(|| contract("anchor has no enclosing native closure"))
    }

    fn return_object(&self, function: Scope, containing: usize) -> Result<Scope, RewriteError> {
        let mut found = Vec::new();
        for i in function.open + 1..function.close {
            if self.tokens[i].text == "return" && self.tokens[i + 1].text == "{" {
                let close = self.pair(i + 1)?;
                if i < containing && containing < close && close < function.close {
                    found.push(Scope { open: i + 1, close });
                }
            }
        }
        if found.len() != 1 {
            return Err(contract("native capability is not in one returned object"));
        }
        Ok(found[0])
    }

    fn methods(&self, object: Scope) -> Result<Vec<Method>, RewriteError> {
        let mut methods = Vec::new();
        let mut i = object.open + 1;
        while i < object.close {
            if self.tokens.get(i + 1).is_some_and(|t| t.text == ":")
                && self.tokens.get(i + 2).is_some_and(|t| t.text == "function")
            {
                let open = self.pair(i + 3)? + 1;
                let close = self.pair(open)?;
                methods.push(Method {
                    name: self.tokens[i].text.trim_matches('"').to_owned(),
                    body: Scope { open, close },
                });
                i = close + 1;
            } else if matches!(self.tokens[i].text, "(" | "[" | "{") {
                i = self.pair(i)? + 1;
            } else {
                i += 1;
            }
        }
        Ok(methods)
    }

    fn method_at(&self, object: Scope, at: usize) -> Result<Method, RewriteError> {
        self.methods(object)?
            .into_iter()
            .find(|method| method.body.open < at && at < method.body.close)
            .ok_or_else(|| contract("anchor has no native method"))
    }

    fn returned_by(&self, object: Scope, value: &str) -> Result<String, RewriteError> {
        let found: Vec<_> = self
            .methods(object)?
            .into_iter()
            .filter(|method| {
                let body: Vec<_> = self.tokens[method.body.open + 1..method.body.close]
                    .iter()
                    .map(|t| t.text)
                    .collect();
                body == ["return", value] || body == ["return", value, ";"]
            })
            .collect();
        if found.len() != 1 {
            return Err(contract("native target getter changed"));
        }
        Ok(found[0].name.clone())
    }

    fn assigned_owner(&self, scope: Scope) -> Result<String, RewriteError> {
        if scope.open < 4
            || self.tokens[scope.open - 3].text != "function"
            || self.tokens[scope.open - 4].text != "="
        {
            return Err(contract("native module assignment changed"));
        }
        let owner = &self.tokens[scope.open - 5];
        if !owner.identifier {
            return Err(contract("native module owner is not an identifier"));
        }
        Ok(owner.text.to_owned())
    }

    fn assignment(&self, object: Scope, code: String) -> (usize, String) {
        // Insert before the return keyword, so this assignment retains the
        // module's private variables without publishing an arbitrary registry.
        (self.tokens[object.open - 1].start, code)
    }

    fn expand(&self, text: &str) -> String {
        let mut out = text.to_owned();
        let mut keys: Vec<_> = self.names.keys().collect();
        keys.sort_by_key(|key| std::cmp::Reverse(key.len()));
        for key in keys {
            out = out.replace(key, &self.names[key]);
        }
        out
    }
}

/// Rewrite the current Web3 client, or report exactly which native capability
/// could not be established. The caller alone chooses notification policy.
pub fn rewrite_client(src: &str) -> Result<String, RewriteError> {
    if src.contains(MARKER) {
        return if src.matches(MARKER).count() == 1 && src.contains(PRODUCER) {
            Ok(src.to_owned())
        } else {
            Err(contract("partial or duplicate Web3 instrumentation"))
        };
    }
    let protocol_tags = ["\"NEXTREADY\"", "\"RYUUKYOKU\"", "\"GO\""]
        .iter()
        .filter(|tag| src.contains(**tag))
        .count();
    if protocol_tags < 2 && !(src.contains("\"[name=ok]\"") && src.contains("\"LOBBY\"")) {
        return Err(RewriteError::NotClient);
    }
    if src.contains("__washizuWeb3") {
        return Err(contract("producer namespace already exists"));
    }
    let mut s = Source::new(src)?;
    let discard = s.unique("physical discard", r#"function __discard(__tileArg,__forcedArg){var __message={tag:"D",p:__tileArg};if(1==__state.__mode){if(!__forcedArg&&(__forcedArg=__clock.__mounted(),__clear(),!__forcedArg))return;__wire.__send(__message)}else 4==__state.__mode&&(__clear(),__popup.__training(__message));__message[0]=__tileArg;__wire.__apply(__message)}"#)?;
    let input_scope = s.enclosing_function(discard)?;
    let slot = s.unique("discard hand and actor", r#"__slotMethod:function(__slotArg,__slotForced){var __physical=__lastTile;__hand[64]||(__physical=__hand[32|__actor][__slotArg]);__discard(__physical,__slotForced)}"#)?;
    let input_object = s.return_object(input_scope, slot)?;
    s.unique("legal discard marks", r#"function __clear(__event){if(1==__state.__mode||4==__state.__mode)if(!__popup.__element||__popup.__element.style.display){var __marks=[0,0,0,0,0,0,0,0,0,0,0,0,0,0];if(__event)if("REACH"==__event.tag)for(var __waits=__rules.__waitMethod(),__markSlot=0;__markSlot<__hand[32].length;++__markSlot){var __wait=__waits[__hand[32][__markSlot]>>2];__marks[__markSlot]|=__wait&&__wait.length?__flags.__legal:__flags.__blocked}"#)?;
    s.unique("native card input guard", r#"else if(__focus instanceof __card)__bits=__cards[1024|__focus.__slotKey].__cardFlags,(__menu[2360326].parentNode.style.visibility||__menu[2360326].parentNode.style.display)&&__bits&__flags.__legal&&__focus.__present&&(__state.__slotMethod(__focus.__slotKey),__select());else if"#)?;
    let mouse = s.unique("viewport mouse coordinates", r#"mousemove:function(__moveEvent){var __moveNode=__moveEvent.target;if(void 0!==__moveEvent.buttons?2!=__moveEvent.buttons:3!=__moveEvent.which){__pageX=__moveEvent.pageX;__pageY=__moveEvent.pageY;var __localX=__pageX-__stage.offsetLeft-__table.__canvasContext.canvas.offsetLeft,__localY=__pageY-__topOffset;__pointerBits&=-3;__hitTest(__moveNode,__localX,__localY,!1,void 0!==__moveEvent.buttons?1==__moveEvent.buttons:1==__moveEvent.which)}}"#)?;
    let pointer_scope = s.enclosing_function(mouse)?;
    s.unique("native mouse hit-test parameters", r#"function __hitTest(__hitNode,__hitX,__hitY,__touchFallback,__dragging,__nativeEvent){"BUTTON"==__hitNode.tagName||__hitNode.parentNode!=__handPanel.__panelRoot||__handPanel[20].style.display?"#)?;
    s.unique("native mouse scale", r#"__hitX*=__ratio;__hitY*=__ratio;var __hitCard;if(__dragging&&__focus instanceof __card)"#)?;
    s.unique("native card hit region", r#"else for(__hitNode=0;(__hitCard=__cards[1024|__hitNode])&&!(__hitCard.__cardX<=__hitX&&__hitX<__hitCard.__cardX+__layout.__cardWidths[4]&&__hitCard.__cardY<=__hitY&&__hitY<__hitCard.__cardY+__layout.__cardHeights[4]+__layout.__cardExtras[4]);++__hitNode);"#)?;
    s.unique("native menu mouse route", r#"else if(__hitNode==__status.__statusContext.canvas)__select(__hitNode);else if(__hitNode.parentNode&&__hitNode.parentNode==__menu.__menuRoot)__select(("SELECT"==__hitNode.tagName?__hitNode.previousSibling:__hitNode).firstChild);else{"#)?;
    s.unique("native menu DOM identity", r#"__menu[__createdCode]=__nodeBuilder("div",__menuOuter,"tbc"+(__createdCode&2097152?" bblink":""),{id:"m"+__createdCode,innerHTML:__menuText[__menuIndex+1]}"#)?;
    s.unique("native menu reveal routes", r#"case 3670533:__menu.__switchMenu(8192);break;case 3671045:__menu.__switchMenu(16384);break;"#)?;

    let menu = s.unique("native offered action", r#"__chooseMethod:function(__code){if(4==__state.__mode)__clear(),__popup.__training(__offers[__code]);else if(__offers[__code]){var __wasReady=__clock.__mounted();__clear();if(!__wasReady)return;__wire.__send(__offers[__code])}__offers={}}"#)?;
    let menu_scope = s.enclosing_function(menu)?;
    let menu_object = s.return_object(menu_scope, menu)?;
    // Validate the module boundary even though playing handlers are never exported.
    s.assigned_owner(menu_scope)?;
    s.unique("native claim and pass offers", r#"__claimMethod:function(__claimMask){if(1==__state.__mode||4==__state.__mode)__offers={2360326:{tag:"N"}},__claimMask&__operationBits.__operationEnum.__ronBit&&(__offers[2360328]={tag:"N",type:6})"#)?;
    let clock = s.unique(
        "native mounted clock",
        r#"__mounted:function(){return!!__clockElement.parentNode}"#,
    )?;
    let clock_scope = s.enclosing_function(clock)?;
    let clock_object = s.return_object(clock_scope, clock)?;
    if s.assigned_owner(clock_scope)? != s.name("__clock") {
        return Err(contract("clock owner mismatch"));
    }
    let clock_open = s.unique("clock opening", r#"__openClock:function(__duration,__clockTarget,__clockMove){__clockElement.firstChild.innerHTML="";__clockTarget||(__clockTarget=__query("[name=ok]"));__clock.__positionClock(__clockTarget,__clockMove);1==__state.__mode&&"#)?;
    let clock_started = s.unique("native clock counter initialization", r#"appendChild(__clockElement),0>__duration?__countdown=-__duration+1E3:__countdown=(__duration||__fixedMs)+__bankMs+1E3,__countdownLimit=__countdown-1E3,__clockRemaining=__countdown-1E3"#)?;
    let clock_close = s.unique(
        "clock withdrawal",
        r#"__target=void 0;__clockRemaining=-1;delete __ticks().count"#,
    )?;
    let target_getter = s.returned_by(clock_object, s.name("__target"))?;
    let close_method = s.method_at(clock_object, clock_close)?;
    let open_method = s
        .methods(clock_object)?
        .into_iter()
        .find(|m| m.name == s.name("__openClock"))
        .ok_or_else(|| contract("clock opening method is outside clock module"))?;
    if !(clock_object.open < clock_open && clock_open < clock_object.close) {
        return Err(contract("clock opening scope changed"));
    }

    let modal = s.unique(
        "modal overlay",
        r#"__hasModal:function(){return!!__modalElement}"#,
    )?;
    let modal_scope = s.enclosing_function(modal)?;
    let modal_owner = s.assigned_owner(modal_scope)?;
    let modal_object = s.return_object(modal_scope, modal)?;
    let disconnected = s.unique("current socket disconnect dialog", DISCONNECT_DIALOG)?;
    if s.name("__disconnectModal") != modal_owner {
        return Err(contract("disconnect dialog owner changed"));
    }
    let modal_show = s.unique("modal confirmation callback", r#"__showModal:function(__modalMessage,__modalButtons,__modalConfirm,__modalCancel,__modalCountdown,__modalOkLabel,__modalCancelLabel){__modalOkLabel=__modalOkLabel||"OK";__modalCancelLabel=__modalCancelLabel||"CANCEL";1==arguments.length&&(__modalButtons=1);__modalCallbacks=[__modalCancel,__modalConfirm];"#)?;
    s.method_at(modal_object, s.pair(modal_show + 3)? + 2)?;
    let close_body = disconnected + 5;
    let callback = (close_body + 1..s.pair(close_body)?)
        .find(|&i| s.tokens[i].text == "function")
        .ok_or_else(|| contract("disconnect confirmation callback missing"))?;
    let callback_end = s.pair(s.pair(callback + 1)? + 1)?;
    s.unique("registered title session", r##"__titleElement.innerHTML=__titleHtml;__bindTitle(__titleElement);__showTitle(-1);__titleNodes["pane"+(__titleRemote?2:__titleStorage[__titleKeys.__titleLoginKey]?0:1)].style.display="";__popup["#sx"]();if("AUTOLOGIN"==__titleEvent.tag||__titleStorage[__titleKeys.__titleAutoLogin])__popup.__element.style.display="none",__popup.ok()"##)?;
    if !s
        .methods(modal_object)?
        .iter()
        .any(|m| m.name == s.name("__hasModal"))
    {
        return Err(contract("modal getter changed"));
    }

    let popup = s.unique("popup queue", r#""AGARI"==__currentPopup.tag&&"AGARI"==__popupEvent.tag?__popupQueue.push(__popupEvent):__popup.__replacePopup(__popupEvent,__popupCallback);else{__currentPopup=__popupEvent;__popupEvent=__popup.__element="#)?;
    let popup_method_scope = s.enclosing_function(popup)?;
    let popup_scope = s.enclosing_function(popup_method_scope.open)?;
    let popup_object = s.return_object(popup_scope, popup)?;
    let popup_show = s.method_at(popup_object, popup)?;
    let popup_replace = s
        .methods(popup_object)?
        .into_iter()
        .find(|m| m.name == s.name("__replacePopup"))
        .ok_or_else(|| contract("popup withdrawal changed"))?;
    if !s.methods(popup_object)?.iter().any(|m| m.name == "ok") {
        return Err(contract("native OK handler is missing"));
    }
    let popup_new = s.unique(
        "popup instantiation",
        r#"__currentPopup=__popupEvent;__popupEvent=__popup.__element="#,
    )?;

    let selected = s.unique("selected lobby rules", r#"function __selected(){return(""+(__storage[__storageKeys.__ruleKey]||__defaultRules)).split(",")}"#)?;
    let lobby_scope = s.enclosing_function(selected)?;
    let join = s.unique("lobby reservation send", r#"__dom.cancel.style.display="";__dom.logout.style.display="none";__wire.__send({tag:"JOIN",t:__lobby+","+__joinMode});__reservations[__joinMode]=1"#)?;
    let lobby_object = s.return_object(lobby_scope, join)?;
    s.unique("lobby asset readiness", r#"function __loaded(__loadWarn){if(__images[4096]&&__images[4096].complete&&__images[4196]&&__images[4196].complete&&__images[4296]&&__images[4296].complete&&__images[4396]&&__images[4396].complete&&__images[4496]&&__images[4496].complete)return!0;__loadWarn&&"#)?;
    if !s.methods(lobby_object)?.iter().any(|m| m.name == "join") {
        return Err(contract("native join handler is missing"));
    }

    let mut edits: Vec<(usize, String)> = Vec::new();
    let first_fn = s
        .tokens
        .iter()
        .position(|t| t.text == "function")
        .ok_or_else(|| contract("missing outer IIFE"))?;
    if s.tokens[first_fn + 1].text != "(" {
        return Err(contract("outer IIFE has a name"));
    }
    let outer_open = s.pair(first_fn + 1)? + 1;
    let outer_close = s.pair(outer_open)?;
    if s.tokens[outer_open].text != "{"
        || outer_close + 4 >= s.tokens.len()
        || s.tokens[outer_close + 1..outer_close + 5]
            .iter()
            .map(|t| t.text)
            .collect::<Vec<_>>()
            != [")", "(", ")", ";"]
    {
        return Err(contract("outer IIFE boundary changed"));
    }
    edits.push((s.tokens[outer_open].end, format!("var {EPOCH}=0,{UI_EPOCH}=0,__washizuWeb3PhaseReady=false,__washizuWeb3Closing=false,__washizuWeb3ClockStarted=null,__washizuWeb3ClockFixed=null,__washizuWeb3ClockBank=null,__washizuWeb3Clock,__washizuWeb3Input,__washizuWeb3Menu,__washizuWeb3Popup,__washizuWeb3Lobby,__washizuWeb3Geometry,__washizuWeb3Disconnect;var __washizuWeb3DisconnectCallbacks=new WeakSet();function __washizuWeb3TagDisconnect(callback){{__washizuWeb3DisconnectCallbacks.add(callback);return callback}}")));
    // Tag the original callback by identity; never invoke or replace its body.
    edits.push((
        s.tokens[callback].start,
        "__washizuWeb3TagDisconnect(".into(),
    ));
    edits.push((s.tokens[callback_end].end, ")".into()));
    edits.push(s.assignment(modal_object, s.expand("__washizuWeb3Disconnect={read:function(){return __modalCallbacks&&__washizuWeb3DisconnectCallbacks.has(__modalCallbacks[1])?__modalElement:null}};")));
    edits.push((s.tokens[clock_started + 5].start, s.expand("__washizuWeb3ClockStarted=performance.now(),__washizuWeb3ClockFixed=__duration||__fixedMs,__washizuWeb3ClockBank=__bankMs,")));
    edits.push(s.assignment(clock_object, s.expand("__washizuWeb3Clock={facts:function(){return{startedAt:__washizuWeb3ClockStarted,now:performance.now(),fixedMs:__washizuWeb3ClockFixed,bankMs:__washizuWeb3ClockBank,remainingMs:Math.max(0,__countdown-1000)}}};")));

    let native_input = s.expand(r#"__washizuWeb3Input={mode:function(){return __state.__mode},phaseReady:function(){return __washizuWeb3PhaseReady},hand:function(){return __hand[32]},actor:function(){return __actor},drawn:function(){return __lastTile},riichi:function(){return!!__hand[64]},isCard:function(v){return v instanceof __card},targetSlot:function(v){return v.__slotKey},slot:function(i){var c=__cards[1024|i];return c&&{target:c,present:!!c.__present,legal:!!(c.__cardFlags&__flags.__legal),blocked:!!(c.__cardFlags&__flags.__blocked),x:c.__cardX,y:c.__cardY}}};"#);
    edits.push(s.assignment(input_object, native_input));
    edits.push(s.assignment(menu_object, s.expand("__washizuWeb3Menu={offers:function(){return __offers},responseTarget:function(){return __menu[2360326]},control:function(code){return __menu[code]},reveal:function(code){return code&8192?__menu[3670533]:code&16384?__menu[3671045]:null}};")));
    let pointer_return = s.unique("pointer observer return", r#"return{__pointerRefresh:function(){if(!(__focus instanceof __card)){var __underPointer=__document.elementFromPoint(~~(__pageX-__browser.pageXOffset),~~(__pageY-__browser.pageYOffset));__underPointer&&__hitTest(__underPointer,__pageX-__stage.offsetLeft-__table.__canvasContext.canvas.offsetLeft,__pageY-__topOffset,!1)}}}"#)?;
    let pointer_object = Scope {
        open: pointer_return + 1,
        close: s.pair(pointer_return + 1)?,
    };
    if pointer_object.open <= pointer_scope.open || pointer_object.close >= pointer_scope.close {
        return Err(contract("pointer observer scope changed"));
    }
    edits.push(s.assignment(pointer_object, s.expand("__washizuWeb3Geometry={read:function(){return{ratio:__ratio,stage:__stage,canvas:__table.__canvasContext.canvas,statusCanvas:__status.__statusContext.canvas,left:__stage.offsetLeft+__table.__canvasContext.canvas.offsetLeft,top:__topOffset,width:__layout.__cardWidths[4],height:__layout.__cardHeights[4]+__layout.__cardExtras[4],handPanel:__handPanel.__panelRoot,menuRoot:__menu.__menuRoot}}};")));
    edits.push(s.assignment(popup_object, s.expand("__washizuWeb3Popup={read:function(){return{element:__popup.__element,tag:__currentPopup&&__currentPopup.tag,queued:__popupQueue.length,closing:__washizuWeb3Closing}}};")));
    edits.push(s.assignment(lobby_object, s.expand("__washizuWeb3Lobby={read:function(){return{lobby:__lobby,selected:__selected(),reservationRequested:!!Object.keys(__reservations).length,loaded:__loaded(false)}}};")));

    // Native lifecycle calls invalidate identities even if nobody polls during
    // a close/reopen cycle, or if the client reuses every tile/clock DOM node.
    // Raw end-of-hand frames retire a retained clock before delayed result
    // animation starts. Only an actual native clock opening restores readiness.
    // An opponent disconnect only updates the connectivity mask and status
    // painting. Verify its entire body before excluding it from input identity:
    // otherwise BYE arriving during pacing cancels our unchanged legal action.
    let disconnect = s.unique("disconnect metadata callback", DISCONNECT_METADATA)?;
    // The anchor starts at the property key; method_at requires a body token.
    let disconnect = s.method_at(input_object, s.pair(disconnect + 3)? + 2)?;
    for method in s.methods(input_object)? {
        if method.body.open == disconnect.body.open {
            continue;
        }
        let invalidate = if matches!(
            method.name.as_str(),
            "T" | "D" | "N" | "REACH" | "AGARI" | "RYUUKYOKU" | "GO" | "TAIKYOKU" | "INIT"
        ) {
            "__washizuWeb3PhaseReady=false;"
        } else {
            ""
        };
        edits.push((
            s.tokens[method.body.open].end,
            format!("++{EPOCH};{invalidate}"),
        ));
    }
    for method in s.methods(menu_object)? {
        let body = &s.tokens[method.body.open + 1..method.body.close];
        let rebuilds = body.windows(3).any(|tokens| {
            tokens[0].text == s.name("__offers") && tokens[1].text == "=" && tokens[2].text == "{"
        });
        let invalidate = if rebuilds || method.name == s.name("__chooseMethod") {
            "__washizuWeb3PhaseReady=false;"
        } else {
            ""
        };
        edits.push((
            s.tokens[method.body.open].end,
            format!("++{EPOCH};{invalidate}"),
        ));
    }
    for (method, ready) in [(open_method, true), (close_method, false)] {
        edits.push((
            s.tokens[method.body.open].end,
            format!("++{EPOCH};__washizuWeb3PhaseReady={ready};"),
        ));
    }
    edits.push((
        s.tokens[popup_show.body.open].end,
        format!("++{UI_EPOCH};++{EPOCH};__washizuWeb3PhaseReady=false;"),
    ));
    edits.push((
        s.tokens[popup_replace.body.open].end,
        format!("__washizuWeb3Closing=true;++{UI_EPOCH};++{EPOCH};__washizuWeb3PhaseReady=false;"),
    ));
    edits.push((
        s.tokens[popup_new].start,
        "__washizuWeb3Closing=false;".into(),
    ));
    for method in s.methods(lobby_object)? {
        let bump = match method.name.as_str() {
            "join" | "cancel" | "rule" => format!("++{UI_EPOCH};"),
            "#rule0" => format!("if(arguments.length)++{UI_EPOCH};"),
            _ => continue,
        };
        edits.push((s.tokens[method.body.open].end, bump));
    }
    // Only the public ID prefix leaves the page; never its secret suffix.
    let native = s.expand(&format!("{{epoch:function(){{return {EPOCH}}},uiEpoch:function(){{return {UI_EPOCH}}},input:__washizuWeb3Input,menu:__washizuWeb3Menu,popup:__washizuWeb3Popup,lobby:__washizuWeb3Lobby,disconnect:__washizuWeb3Disconnect,title:{{read:function(){{var id=__titleStorage[__titleKeys.__titleLoginKey];return{{account:typeof id==='string'&&/^ID[0-9A-F]{{8}}-[0-9A-Za-z]{{8}}$/.test(id)?id.substr(0,10):null}}}}}},geometry:__washizuWeb3Geometry,clock:{{mounted:function(){{return __clock.__mounted()}},target:function(){{return __clock.{target_getter}()}},facts:function(){{return __washizuWeb3Clock.facts()}}}},modal:function(){{return {modal_owner}.__hasModal()}}}}"));
    edits.push((
        s.tokens[outer_close].start,
        format!(";{MARKER}window.__akagiTenhou=({PRODUCER})({native});\n"),
    ));
    edits.sort_by_key(|(position, _)| *position);
    if edits.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(contract("instrumentation scopes overlap"));
    }
    let mut out = String::with_capacity(src.len() + PRODUCER.len() + 4096);
    let mut previous = 0;
    for (position, insertion) in edits {
        out.push_str(&s.code[previous..position]);
        out.push_str(&insertion);
        previous = position;
    }
    out.push_str(&s.code[previous..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_are_not_main_clients() {
        assert_eq!(
            rewrite_client("window.inflate=function(){}"),
            Err(RewriteError::NotClient)
        );
    }

    #[test]
    fn main_client_with_missing_capabilities_is_a_contract_error() {
        assert!(matches!(
            rewrite_client("(function(){var a=[\"GO\",\"RYUUKYOKU\",\"NEXTREADY\"]})();"),
            Err(RewriteError::Contract(_))
        ));
        assert!(matches!(
            rewrite_client("(function(){var a=[\"BROKEN_GO\",\"RYUUKYOKU\",\"NEXTREADY\"]})();"),
            Err(RewriteError::Contract(_))
        ));
    }

    #[test]
    fn scanner_keeps_regex_braces_out_of_scope_pairs() {
        let source =
            Source::new("(function(){var x=/[{}]\\/[a-z]+/g;return {a:function(){return x}}})();")
                .unwrap();
        assert_eq!(source.tokens.iter().filter(|t| t.text == "{").count(), 3);
    }

    #[test]
    fn unique_shapes_bind_names_and_refuse_ambiguity() {
        let mut source =
            Source::new("function Ab(t){return t.x}function Cd(t){return t.x}").unwrap();
        assert!(source
            .unique("ambiguous", "function __f(__a){return __a.__p}")
            .is_err());
        let mut source = Source::new("function Renamed(tile){return tile.flags}").unwrap();
        source
            .unique("renamed", "function __f(__a){return __a.__p}")
            .unwrap();
        assert_eq!(source.name("__f"), "Renamed");
        assert_eq!(source.name("__p"), "flags");
    }

    #[test]
    fn disconnect_metadata_exemption_requires_the_complete_status_only_body() {
        let body = "BYE:function(e){s.connected&=~(1<<~~e.who);ui.draw();return 1}";
        let mut source = Source::new(body).unwrap();
        assert!(source
            .unique("disconnect metadata callback", DISCONNECT_METADATA)
            .is_ok());
        for changed in [
            body.replace("ui.draw();", "ui.draw();clock.close();"),
            body.replace("ui.draw();", "ui.draw();hand.pop();"),
            body.replace("connected&=", "connected|="),
        ] {
            let mut source = Source::new(&changed).unwrap();
            assert!(source
                .unique("disconnect metadata callback", DISCONNECT_METADATA)
                .is_err());
        }
    }

    #[test]
    fn disconnect_dialog_requires_the_owned_socket_and_original_reload_confirmation() {
        let native = r#"close:function(){this==socket&&(queue=[],modal.show("CONNECTION CLOSED<br>"+tr(strings.retry),3,function(){location.reload()}))}"#;
        let mut source = Source::new(native).unwrap();
        source.unique("disconnect", DISCONNECT_DIALOG).unwrap();
        assert_eq!(source.name("__disconnectModal"), "modal");
        for damaged in [
            native.replace("this==socket&&", ""),
            native.replace("location.reload()", "location.replace('/3/')"),
            native.replace("CONNECTION CLOSED", "OTHER ERROR"),
            native.replace("),3,function", "),1,function"),
        ] {
            assert!(Source::new(&damaged)
                .unwrap()
                .unique("disconnect", DISCONNECT_DIALOG)
                .is_err());
        }
    }
}
