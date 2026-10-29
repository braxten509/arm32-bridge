# Offline play: games that call home get their answers from the phone

Owner (2026-10-29): "All games are made for offline - we need to disable online parts via the
bridge." First game: Farm Story 1.9.6.4 (TeamLava/Storm8), a thin client whose farm, shop and
prices all live on its server. Away mode: decisions are logged here and in
~/dev/arm32-bridge-games/farmstory/OFFLINE-LOG.md.

## Goal and checks

- A converted game whose phone Network switch is OFF reaches play, saves progress on the
  phone, and keeps it after a restart. For Farm Story: farm loads, plow/plant/harvest/tree
  work, coins/XP change, restart shows the same farm.
- Nothing leaves the phone: no real socket is opened for an offline host.
- General: the network layer knows no game. A game family's server stand-in (Storm8 first)
  is a separate module, picked by the host name the game asks for.
- Tests first: PC unit tests for the virtual network (fd table, name lookup, connect, HTTP
  framing) and for the Storm8 stand-in (login from the recorded catalog, applyChanges
  updating the saved farm, restart answer equal in shape to the recorded second login).
  Each test gets a planted mistake. Phone check as above, 0 translator fallbacks.

## Design (revised after review, 2026-10-29)

First idea (dropped): patch ART's libjavacore/libopenjdk socket and DNS imports and fake TCP
sockets with AF_UNIX pairs. Terra's review: socket tagging/BlockGuardOs, IPv6-first logic,
non-blocking connect + SO_ERROR, DNS caches and other paths make that fragile. Kept as a
fallback idea for games whose native code or other HTTP stacks need it.

Chosen: answer at the HTTP library, before any socket or name lookup exists.

### 1. Apache HttpClient (Fix 7, converter/src/dex.rs)

Old games build their client with `new DefaultHttpClient(...)` (Farm Story: StormApiBase,
DownloadManager, ImageLoader, ExecutablePromotionAd; GetJar extends it). The converter can't
add method references to a dex, but it can rename a type in place: the descriptor string
`Lorg/apache/http/impl/client/DefaultHttpClient;` becomes
`Lorg/apache/http/impl/client/OfflineHttpClient;` (same length; the fix checks it still sorts
between its neighbours, which ART's verifier requires, and that no method of a class outside
the dex takes or returns the type). Every `new`, constructor call and subclass in the game now
means Game Bridge's OfflineHttpClient (extra classesN.dex, converter/assets/offline.dex,
tools/build-offline-dex.sh), a DefaultHttpClient subclass with the same constructors whose
createClientRequestDirector (the extension point behind AbstractHttpClient's final execute
methods) returns a director that hands the request to the bridge and returns its answer.
Only done when the game gets an offline pack.

### 2. Java -> bridge (src/offline/mod.rs)

dev.braxten.gamebridge.Offline.answer(host, port, method, uri, headers[], body) -> byte[]
(status line, headers, body), a native method in libarm32_bridge.so (the class loads it with
System.loadLibrary; the stand-in has loaded it already). null = no offline answer: the director
throws UnknownHostException, what the game sees with no network.
Later, for HttpURLConnection games: URL.setURLStreamHandlerFactory from an added provider.

### 3. Answer sets

An answer set maps host names to handlers:
- files: a host whose requests are served from a folder of mirrored files (path -> file,
  query ignored); missing file -> 404.
- storm8: the Storm8/TeamLava game API stand-in (src/offline/storm8.rs, section 4).
Which hosts map to what is data in the game's offline pack, not code.

### 4. Storm8 stand-in (src/offline/storm8.rs)

From PROTOCOL.md (~/dev/arm32-bridge-games/farmstory) and the recorded traffic:
- login: the recorded first-login gameData (catalog/systemData, constants), with the user,
  board and inventory from the saved state (first start: the recorded new-user state).
  Session token made up; the client's hash (sh) is not checked.
- applyChanges: the client sends change events ("seq,time,kind,..." lines in
  piggyBackedInfo); each is applied to the saved state (cash, XP, level, board items and
  their timers, tutorial state), then the answer carries the new totals like the server's.
- Every other method: the recorded answer when there is one, else {"callResult":{"success":true}}
  with the empty lists the client expects (ads, news, neighbors, gifts).
- State is saved after every change to <app files>/gamebridge-offline/<host>.json
  (write to .tmp, then rename).

### 5. Offline pack (data, never in git)

The recorded answers and the mirrored CDN pictures belong to the game's maker, so they stay
in ~/dev/arm32-bridge-games/<game>/offline-pack and go only into the converted APK:
`tools/convert.sh game.apk --offline-pack <dir>` stores the pack as
lib/<abi>/libarm32_bridge_offline.so (a zip, named lib*.so so Android unpacks it next to the
bridge, like libarm32_bridge_settings.so). pack.toml lists hosts and their handler kind.
Game Bridge's own converter (phone) can take a pack sitting next to the APK later.

### 6. Where it starts

The first Offline.answer call opens the pack (lib dir, libarm32_bridge_offline.so) and logs
"[offline] N hosts, M files". The pack is a small archive of Game Bridge's own (index + blobs;
the bridge has no zip reader): magic "GBOFFLN1", entry count, then name/offset/size entries.
