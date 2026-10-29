//! IOS_PHONE_ANIM=1: animations run on the phone, as Core Animation's
//! render server runs them. While layers move only by place, turn and
//! opacity, a list carries a scene: the views as built (immutable, their
//! animations as plain data, `LiveAnim`) and the files every picture in it
//! is drawn from. The phone asks for the frame of each refresh (`frame`,
//! from its own thread, through JNI): worked out here from the scene alone,
//! with no objects, no game thread and no new pictures, and handed over as
//! a few numbers per picture (`Scene::frame_bytes`).

use super::*;

/// What a scene can't hold: the game thread lists those frames itself.
pub(super) type NotLive = std::borrow::Cow<'static, str>;

pub(crate) struct Scene {
    /// The windows as built, each with the area it may draw in.
    windows: Vec<(Arc<Node>, Rect)>,
    /// The windows' turn on the screen (an old app's, uikit::window_turn).
    turn: Option<Transform>,
    /// The files its pictures are drawn from (their numbers in a frame).
    pub(super) files: Vec<String>,
    by_path: HashMap<String, u32>,
    texts: Vec<(crate::fw::graphics::TextLook, u32)>,
    /// Its still layers drawn as one picture, as made when it was built.
    still: Arc<StillPictures>,
    /// When the last animation in it stops changing what shows (media time).
    end: f64,
    /// The seam gauge's layer (IOS_DRAW_LOG=1): where it is goes with each frame.
    card: Option<Id>,
}

/// Why `nodes` can't be a scene (None: they can): what changes in them other
/// than by place, turn and opacity. The scene's evaluator moves only layers
/// with animations and what they hold (`moved`: it or a layer around it
/// animates), so a still layer is in the same place at the same opacity at
/// every moment. A GL or video view may then be in one: the phone keeps
/// showing its frames under the pictures (the list's GL line stays right
/// for the whole scene). So may a layer drawn as one picture here (a mask,
/// shadow, rounded clip, rasterized layer) with nothing inside it
/// animating: its picture is made once with the scene (`Scene::build`).
/// One that moves or holds the animation keeps the list baked.
fn not_live(node: &Node, moved: bool) -> Option<NotLive> {
    let moved = moved || node.live.is_some();
    if node.changing || node.ticking {
        return Some("a view that changes by itself".into());
    }
    if (node.gl || node.video) && moved {
        return Some("a GL or video view that moves or fades".into());
    }
    if !node.picture_motion.is_empty() {
        return Some("a contents animation".into());
    }
    if let Some(kind) = one_picture(node) {
        let why = if moved {
            "that moves"
        } else if inside(node).any(|n| n.live.is_some() || n.layer != 0 && crate::fw::animation::animating(n.layer)) {
            "with an animation inside"
        } else if inside(node).any(|n| n.gl || n.video) {
            // (Its picture can't cut what is under a GL view in it.)
            "with a GL or video view inside"
        } else {
            ""
        };
        if !why.is_empty() {
            return Some(format!("a layer drawn as one picture {why}: {} ({kind})", class_of(node)).into());
        }
    }
    if node.layer != 0 && node.live.is_none() && crate::fw::animation::animating(node.layer) {
        return Some("an animation the phone can't run".into());
    }
    node.children.iter().map(|child| &**child).chain(node.mask.as_deref()).find_map(|child| not_live(child, moved))
}

/// What makes `node` a layer drawn as one picture (`walk` draws it into one,
/// or cuts it to its rounded corners), None when nothing does.
fn one_picture(node: &Node) -> Option<String> {
    let kinds = [(node.mask.is_some(), "mask"), (node.shadow.is_some(), "shadow"), (node.rasterized, "rasterized"), (node.clips && node.radius > 0.0, "rounded clip")];
    let kinds: Vec<&str> = kinds.iter().filter(|(is, _)| *is).map(|(_, kind)| *kind).collect();
    (!kinds.is_empty()).then(|| kinds.join(", "))
}

/// Every layer inside `node` (its mask's too), not `node` itself.
fn inside(node: &Node) -> impl Iterator<Item = &Node> {
    let mut left: Vec<&Node> = node.children.iter().map(|child| &**child).chain(node.mask.as_deref()).collect();
    std::iter::from_fn(move || {
        let next = left.pop()?;
        left.extend(next.children.iter().map(|child| &**child).chain(next.mask.as_deref()));
        Some(next)
    })
}

/// The class of the view or layer `node` was built from, for a log line.
fn class_of(node: &Node) -> String {
    if node.id == 0 {
        return "a replicator's copy".into();
    }
    let rt = crate::objc::runtime::lock();
    rt.class_name(rt.class_of(node.id))
}

/// The still layers drawn as one picture in `node` (outermost only).
fn still_nodes(node: &Node, out: &mut Vec<usize>) {
    if one_picture(node).is_some() {
        out.push(node as *const Node as usize);
    } else {
        node.children.iter().for_each(|child| still_nodes(child, out));
    }
}

impl Drop for Scene {
    fn drop(&mut self) {
        crate::fw::graphics::unpin_files(&self.files);
    }
}

impl Scene {
    /// The scene of `nodes` (the windows built for this list), on the game
    /// thread: every picture it may draw is made into a file now, hidden
    /// ones too (a card's face, shown at the turn's middle).
    pub(super) fn build(nodes: &[Arc<Node>]) -> Result<Scene, NotLive> {
        if !TRANSITIONS.with(|t| t.borrow().is_empty()) {
            return Err("a transition".into());
        }
        if let Some(why) = nodes.iter().find_map(|node| not_live(node, false)) {
            return Err(why);
        }
        let mut lives = Vec::new();
        nodes.iter().for_each(|node| live_nodes(node, &mut lives));
        if lives.is_empty() {
            return Err("nothing moves".into());
        }
        let (wide, high, _) = crate::fw::uikit::screen_points();
        let windows = nodes.iter().map(|node| (node.clone(), Rect::new(0.0, 0.0, wide.max(node.frame.size.width), high.max(node.frame.size.height)))).collect();
        let mut every = Vec::new();
        nodes.iter().for_each(|node| every_picture(node, &mut every));
        let (made, waiting) = crate::fw::graphics::noting_unready(|| resolve_each(every.clone()));
        if waiting {
            return Err("pictures still being made".into());
        }
        let mut scene = Scene {
            windows,
            turn: crate::fw::uikit::window_turn(),
            files: Vec::new(),
            // (Held from `keep` until the phone lets the scene go.)
            by_path: HashMap::new(),
            texts: Vec::new(),
            still: Arc::new(HashMap::new()),
            end: lives.iter().map(|live| live.end()).fold(f64::NEG_INFINITY, f64::max),
            card: crate::fw::graphics::draw_log().then(|| seam_layer(crate::fw::runloop::animation_time())).flatten(),
        };
        for (asked, made) in every.into_iter().zip(made) {
            let Some(made) = made else { continue };
            let index = scene.file_index(&made.path);
            match asked.text {
                Some(look) => scene.texts.push((look, index)),
                None => {
                    scene.by_path.insert(asked.path, index);
                }
            }
        }
        // The still layers drawn as one picture: walked once (any moment
        // shows them the same) and drawn as the list draws them.
        let mut keys = Vec::new();
        nodes.iter().for_each(|node| still_nodes(node, &mut keys));
        if !keys.is_empty() {
            STILL.with(|still| still.borrow_mut().record = Some((keys, Vec::new())));
            let _ = scene.pictures_at(crate::fw::runloop::animation_time());
            let walked = STILL.with(|still| still.borrow_mut().record.take()).map_or(Vec::new(), |(_, walked)| walked);
            let mut still = HashMap::new();
            for (key, pictures) in walked {
                let (made, waiting) = crate::fw::graphics::noting_unready(|| compose_groups(resolve(pictures)));
                if waiting {
                    return Err("pictures still being made".into());
                }
                let made: Vec<Picture> = made.into_iter().map(|p| Picture { text: None, ..p }).collect();
                for p in made.iter().filter(|p| !p.path.is_empty() && !p.path.starts_with('\u{1}')) {
                    let index = scene.file_index(&p.path);
                    scene.by_path.insert(p.path.clone(), index);
                }
                if still.insert(key, made).is_some() {
                    return Err("a layer drawn as one picture in two places".into());
                }
            }
            scene.still = Arc::new(still);
        }
        Ok(scene)
    }

    /// The number of `file` among the scene's files (added when new). A
    /// file is held from when it is added, so a file made later in the
    /// build can't push it out of a cache, until the scene goes (`Drop`,
    /// also when the build fails partway).
    fn file_index(&mut self, file: &str) -> u32 {
        match self.files.iter().position(|f| f == file) {
            Some(i) => i as u32,
            None => {
                crate::fw::graphics::pin_files(&[file.to_string()]);
                self.files.push(file.to_string());
                (self.files.len() - 1) as u32
            }
        }
    }

    /// IOS_LIVE_CHECK=1: logs whether the scene shows each of the game
    /// thread's own coming frames (`sampling`, frames sampled from `nodes`)
    /// just as `walk` lists them: files, places to a hundredth of a point,
    /// alpha to a thousandth. (The still layers drawn as one picture are the
    /// scene's on both sides: the list's own frames draw them the same.)
    pub(super) fn check(&self, nodes: &[Arc<Node>], sampling: Sampling) {
        let look = |pictures: Vec<Picture>| -> Vec<String> {
            pictures.into_iter().map(|p| {
                let t = p.place;
                format!("{} {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} {:.3}", p.path.split('\t').next().unwrap_or(""), t.a, t.b, t.c, t.d, t.tx, t.ty, p.alpha)
            }).collect()
        };
        let mut differ = 0;
        for k in 0..sampling.count {
            let listed = look(with_still(Some(self.still.clone()), || nodes.iter().flat_map(|w| flatten_at(w, k)).collect()));
            let live = look(self.pictures_at(sampling.start + k as f64 * sampling.step));
            if listed != live {
                differ += 1;
                if differ == 1 {
                    let at = listed.iter().zip(&live).position(|(a, b)| a != b).unwrap_or(listed.len().min(live.len()));
                    crate::log!("pictures: live check: frame {k} differs at picture {at} of {}/{}: listed {:?}, live {:?}", listed.len(), live.len(), listed.get(at), live.get(at));
                }
            }
        }
        crate::log!("pictures: live check: {} of {} frames agree", sampling.count - differ, sampling.count);
    }

    /// Whether something in it still moves at media time `t`.
    pub(crate) fn moving(&self, t: f64) -> bool {
        t < self.end
    }

    /// The pictures at media time `t` (any thread), as `walk` places them,
    /// with marks (cuts) between them.
    pub(super) fn pictures_at(&self, t: f64) -> Vec<Picture> {
        let before = (LIVE_AT.with(|at| at.replace(Some(t))), PHONE_ANIM.with(|p| p.replace(Some(true))), MARKED.with(|m| m.replace(Vec::new())));
        let mut out = Vec::new();
        with_still(Some(self.still.clone()), || {
            for (node, area) in &self.windows {
                walk(node, 0, self.turn.map(|turn| (Proj::affine(turn), None)), FACING_VIEWER, 1.0, *area, None, &mut out);
            }
        });
        LIVE_AT.with(|at| at.set(before.0));
        PHONE_ANIM.with(|p| p.set(before.1));
        MARKED.with(|m| *m.borrow_mut() = before.2);
        out
    }

    /// The file a walked picture is drawn from (None: it can't be drawn).
    fn file_of(&self, p: &Picture) -> Option<u32> {
        match &p.text {
            Some(look) => self.texts.iter().find(|(l, _)| l == look).map(|(_, i)| *i),
            None => self.by_path.get(&p.path).copied(),
        }
    }

    /// The frame at media time `t` for the phone (any thread), into `out`:
    /// little-endian; a head (i32 1, i32 how many items, i32 1 while
    /// something still moves, f32 x4 the seam gauge's card box or NaN), then
    /// per item a byte: 0 a picture (i32 file, a byte of flags: 1 cut to a
    /// rounded rectangle, 2 with perspective; f32 x16 as a list line's
    /// numbers, then x6 the rectangle, then x3 the perspective, when
    /// flagged), 1 a cut (f32 x8: four corners), 2 the cut's end.
    pub(crate) fn frame_bytes(&self, t: f64, out: &mut Vec<u8>) -> Result<(), NotLive> {
        out.clear();
        let pictures = self.pictures_at(t);
        let f = |out: &mut Vec<u8>, v: f64| out.extend_from_slice(&(v as f32).to_le_bytes());
        out.extend_from_slice(&1i32.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&(self.moving(t) as i32).to_le_bytes());
        let card = self.card.and_then(|layer| self.card_box(layer, t));
        for v in card.map_or([f64::NAN; 4], |r| [r.origin.x, r.origin.x + r.size.width, r.origin.y, r.origin.y + r.size.height]) {
            f(out, v);
        }
        let mut count = 0i32;
        for p in &pictures {
            if let Some(corners) = p.path.strip_prefix(CUT_MARK).filter(|rest| rest.starts_with('\t')) {
                let numbers: Vec<f64> = corners[1..].split('\t').filter_map(|n| n.parse().ok()).collect();
                if numbers.len() != 8 {
                    return Err("a cut without its corners".into());
                }
                out.push(1);
                numbers.iter().for_each(|&v| f(out, v));
            } else if p.path == UNCUT_MARK {
                out.push(2);
            } else if [GROUP_MARK, MASK_MARK, END_MARK].contains(&p.path.as_str()) || p.path.starts_with(TRANSITION_MARK) {
                return Err("a group or transition".into());
            } else {
                let Some(file) = self.file_of(p) else { continue };
                out.push(0);
                out.extend_from_slice(&(file as i32).to_le_bytes());
                out.push(p.corner.is_some() as u8 | (p.perspective.is_some() as u8) << 1);
                let (m, c) = (p.place, p.clip);
                for v in [m.a, m.b, m.c, m.d, m.tx, m.ty, p.area.0, p.area.1, p.alpha, p.mode as f64, p.size.0, p.size.1, c.origin.x, c.origin.y, c.size.width, c.size.height] {
                    f(out, v);
                }
                if let Some(Corner { rect: r, radius }) = p.corner {
                    for v in [r.origin.x, r.origin.y, r.size.width, r.size.height, radius.0, radius.1] {
                        f(out, v);
                    }
                }
                for v in p.perspective.into_iter().flatten() {
                    f(out, v);
                }
            }
            count += 1;
        }
        out[4..8].copy_from_slice(&count.to_le_bytes());
        Ok(())
    }

    /// The screen box of `layer`'s bounds at media time `t` (`layer_box`).
    fn card_box(&self, layer: Id, t: f64) -> Option<Rect> {
        fn find(node: &Node, parent: Option<(Proj, Option<Transform3D>)>, layer: Id) -> Option<Rect> {
            let pose = node.pose(0);
            let own = own_place(node, &pose, parent);
            if node.layer == layer {
                return Some(own.screen_box(pose.bounds));
            }
            let inner = inner_step(node, &pose);
            node.children.iter().find_map(|child| find(child, Some((own, inner)), layer))
        }
        let before = LIVE_AT.with(|at| at.replace(Some(t)));
        let found = self.windows.iter().find_map(|(node, _)| find(node, self.turn.map(|turn| (Proj::affine(turn), None)), layer));
        LIVE_AT.with(|at| at.set(before));
        found
    }
}

/// The animations of `node` and what is under it.
fn live_nodes(node: &Node, out: &mut Vec<Arc<LiveAnim>>) {
    out.extend(node.live.clone());
    node.children.iter().for_each(|child| live_nodes(child, out));
}

/// Every picture `node` and what is under it may draw, hidden ones too
/// (unplaced: only their files count): its background, contents and border
/// as `walk` names them. A still layer drawn as one picture is made apart.
fn every_picture(node: &Node, out: &mut Vec<Picture>) {
    if one_picture(node).is_some() {
        return;
    }
    let size = node.frame.size;
    let over_bounds = |path: String| Shown { path, mode: 0, size: (1.0, 1.0), rect: Rect { origin: Point::default(), size }, caps: None, tile: false, text: None, alpha: 1.0 };
    let background = node.background.filter(|color| color[3] > 0.0 && !node.gl).map(|color| {
        over_bounds(if node.radius > 0.0 { box_path((size.width, size.height), node.radius, color, None) } else { color_path(color) })
    });
    let border = node.border.map(|border| over_bounds(box_path((size.width, size.height), node.radius, [0.0; 4], Some(border))));
    for shown in background.iter().chain(&node.pictures).chain(border.iter()) {
        for (path, _, _, _) in parts(shown) {
            if !out.iter().any(|p: &Picture| p.path == path && p.text == shown.text) {
                out.push(Picture { path, text: shown.text.clone(), ..mark_picture(Rect::default()) });
            }
        }
    }
    node.children.iter().for_each(|child| every_picture(child, out));
}

/// The scenes the phone may still draw (shown, or on their way: sent, being
/// read or decoded), by number, with their list's number once sent. Each
/// holds its files (Scene::file_index) until the phone lets it go.
static SCENES: Mutex<Vec<(u64, Option<u64>, Arc<Scene>)>> = Mutex::new(Vec::new());

/// Scenes kept at most when no screen says what it put up (a PC run).
pub(super) const MOST_SCENES: usize = 8;

fn scenes() -> std::sync::MutexGuard<'static, Vec<(u64, Option<u64>, Arc<Scene>)>> {
    SCENES.lock().unwrap_or_else(|e| e.into_inner())
}

/// Keeps `scene` (and so its files) for the phone; answers its number.
/// `capped`: no screen will let it go (a PC run), so only the last
/// MOST_SCENES are kept.
pub(super) fn keep(scene: Scene, capped: bool) -> u64 {
    static NUMBER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let number = NUMBER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let mut kept = scenes();
    kept.push((number, None, Arc::new(scene)));
    let extra = if capped { kept.len().saturating_sub(MOST_SCENES) } else { 0 };
    let gone: Vec<_> = kept.drain(..extra).collect();
    drop(kept);
    // (Their files go once the last holder of each scene lets it go: `Drop`.)
    drop(gone);
    number
}

/// Scene `scene` went with list `list` (None: it wasn't sent, and goes).
pub(super) fn sent(scene: u64, list: Option<u64>) {
    match list {
        Some(list) => scenes().iter_mut().filter(|(n, _, _)| *n == scene).for_each(|(_, l, _)| *l = Some(list)),
        None => release(scene),
    }
}

/// The phone let scene `scene` go (no longer shown nor on its way): its
/// frames and files go.
pub(crate) fn release(scene: u64) {
    let gone: Vec<_> = {
        let mut kept = scenes();
        let (gone, rest) = std::mem::take(&mut *kept).into_iter().partition(|(n, _, _)| *n == scene);
        *kept = rest;
        gone
    };
    drop(gone);
}

/// The frame of scene `number` at media time `t` (any thread, `Scene::frame_bytes`);
/// false when that scene is gone or can't draw it.
pub(crate) fn frame(number: u64, t: f64, out: &mut Vec<u8>) -> bool {
    let Some(scene) = scenes().iter().find(|(n, _, _)| *n == number).map(|(_, _, s)| s.clone()) else { return false };
    scene.frame_bytes(t, out).is_ok()
}
