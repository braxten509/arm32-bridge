//! Network status, network interfaces and the audio graph.
//! The game is told it is offline; the audio graph is set up but makes no
//! sound yet (step 4).

use crate::symbols::{leak, Registry};

// ----- network reachability: always "not reachable" -----

extern "C" fn reachability_create(_allocator: usize, _target: usize) -> usize {
    leak([0usize; 4])
}

extern "C" fn reachability_get_flags(_target: usize, flags: *mut u32) -> bool {
    if !flags.is_null() {
        unsafe { *flags = 0 };
    }
    true
}

extern "C" fn reachability_true_2(_a: usize, _b: usize) -> bool {
    true
}

extern "C" fn reachability_true_3(_a: usize, _b: usize, _c: usize) -> bool {
    true
}

/// getifaddrs: one Wi-Fi interface "en0" with only a link (hardware)
/// address, 02:00:00:00:00:00 as every iPhone since iOS 7 reports it.
/// Apple's ifaddrs: next, name, flags, addr, netmask, dstaddr, data (56 bytes).
/// Apple's sockaddr_dl: len, family (18), index, type, nlen, alen, slen, data.
extern "C" fn getifaddrs(list: *mut usize) -> i32 {
    if list.is_null() {
        return -1;
    }
    let link = leak([0u8; 20]) as *mut u8;
    let name = crate::symbols::leak_cstr("en0");
    unsafe {
        *link = 20;
        *link.add(1) = 18;
        *(link.add(2) as *mut u16) = 1;
        *link.add(4) = 6;
        *link.add(5) = 3;
        *link.add(6) = 6;
        std::ptr::copy_nonoverlapping(b"en0".as_ptr(), link.add(8), 3);
        let mac = [2u8, 0, 0, 0, 0, 0];
        std::ptr::copy_nonoverlapping(mac.as_ptr(), link.add(11), 6);
        let entry = leak([0usize; 7]) as *mut usize;
        *entry.add(1) = name;
        *(entry.add(2) as *mut u32) = 0x1 | 0x40;
        *entry.add(3) = link as usize;
        *list = entry as usize;
    }
    0
}

extern "C" fn freeifaddrs(_list: usize) {}

/// link_ntoa: "en0:2.0.0.0.0.0" (name, then each address byte in hex).
extern "C" fn link_ntoa(address: *const u8) -> usize {
    thread_local! {
        static TEXT: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    let text = unsafe {
        let name_length = *address.add(5) as usize;
        let address_length = *address.add(6) as usize;
        let data = address.add(8);
        let name = std::slice::from_raw_parts(data, name_length);
        let bytes = std::slice::from_raw_parts(data.add(name_length), address_length);
        let mut text = String::from_utf8_lossy(name).into_owned();
        if !bytes.is_empty() {
            text.push(':');
            text += &bytes.iter().map(|b| format!("{b:x}")).collect::<Vec<_>>().join(".");
        }
        text
    };
    TEXT.with(|cell| {
        let mut buffer = cell.borrow_mut();
        *buffer = text.into_bytes();
        buffer.push(0);
        buffer.as_ptr() as usize
    })
}

pub fn register(r: &mut Registry) {
    r.function("SCNetworkReachabilityCreateWithName", reachability_create as usize);
    r.function("SCNetworkReachabilityCreateWithAddress", reachability_create as usize);
    r.function("SCNetworkReachabilityGetFlags", reachability_get_flags as usize);
    r.function("SCNetworkReachabilitySetCallback", reachability_true_3 as usize);
    r.function("SCNetworkReachabilityScheduleWithRunLoop", reachability_true_3 as usize);
    r.function("SCNetworkReachabilityUnscheduleFromRunLoop", reachability_true_3 as usize);
    r.function("SCNetworkReachabilitySetDispatchQueue", reachability_true_2 as usize);
    r.function("getifaddrs", getifaddrs as usize);
    r.function("freeifaddrs", freeifaddrs as usize);
    r.function("link_ntoa", link_ntoa as usize);
}
