//! Starts a game: set up our stand-ins, load the program, run its
//! start-up code and then its main().

use std::ffi::CString;

pub fn setup() {
    crate::symbols::with(|r| {
        crate::fw::register(r);
        crate::objc::register(r);
        crate::libsystem::register(r);
    });
    crate::log!("{} stand-in symbols ready", crate::symbols::with(|r| r.len()));
}

fn find_program(app_dir: &str) -> Result<String, String> {
    let info = std::path::Path::new(app_dir).join("Info.plist");
    if let Ok(bytes) = std::fs::read(&info) {
        let text = String::from_utf8_lossy(&bytes);
        // XML property list: <key>CFBundleExecutable</key><string>NAME</string>
        if let Some(at) = text.find("<key>CFBundleExecutable</key>") {
            let rest = &text[at..];
            if let (Some(start), Some(end)) = (rest.find("<string>"), rest.find("</string>")) {
                return Ok(format!("{app_dir}/{}", &rest[start + 8..end]));
            }
        }
    }
    let name = std::path::Path::new(app_dir)
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("can't tell the program name")?;
    Ok(format!("{app_dir}/{name}"))
}

pub fn run(app_dir: &str) -> Result<(), String> {
    let app_dir = std::fs::canonicalize(app_dir).map_err(|e| format!("{app_dir}: {e}"))?;
    let app_dir = app_dir.to_string_lossy().into_owned();
    crate::report::seconds_since_start();
    crate::report::install_crash_handler();
    crate::report::schedule_stack_dump();
    crate::report::schedule_test_crash();
    crate::libsystem::threads::note_this_thread();
    setup();
    let program = find_program(&app_dir)?;
    crate::log!("loading {program}");
    let loaded = crate::loader::load(&app_dir, &program)?;
    crate::log!(
        "loaded at {:#x}..{:#x} (moved by {:#x}), {} rebases, {} binds",
        loaded.start,
        loaded.end,
        loaded.slide,
        loaded.image.rebases.len(),
        loaded.image.binds.len()
    );
    crate::objc::runtime::run_load_methods();

    let argv_strings = [CString::new(program.clone()).unwrap()];
    let argv: Vec<usize> = argv_strings.iter().map(|s| s.as_ptr() as usize).chain([0]).collect();
    let envp: Vec<usize> = vec![0];
    let apple_strings = [CString::new(format!("executable_path={program}")).unwrap()];
    let apple: Vec<usize> = apple_strings.iter().map(|s| s.as_ptr() as usize).chain([0]).collect();
    let program_vars = [loaded.start, 0, 0, 0, 0];

    if let Some(section) = loaded.image.section("__DATA", "__mod_init_func") {
        let start = section.address.wrapping_add(loaded.slide) as usize;
        let count = section.size as usize / 8;
        crate::log!("running {count} start-up functions");
        for index in 0..count {
            let function = unsafe { *((start + index * 8) as *const usize) };
            let f: extern "C" fn(i32, usize, usize, usize, usize) = unsafe { std::mem::transmute(function) };
            f(1, argv.as_ptr() as usize, envp.as_ptr() as usize, apple.as_ptr() as usize, program_vars.as_ptr() as usize);
        }
        crate::log!("start-up functions done");
    }

    let entry = loaded.image.entry_offset.ok_or("program has no main entry point")?;
    let main_address = loaded.start + entry as usize;
    crate::log!("calling main at {}", crate::loader::describe_address(main_address));
    let main: extern "C" fn(i32, usize, usize, usize) -> i32 = unsafe { std::mem::transmute(main_address) };
    let code = main(1, argv.as_ptr() as usize, envp.as_ptr() as usize, apple.as_ptr() as usize);
    crate::log!("main returned {code}");
    crate::report::finish(code);
}
