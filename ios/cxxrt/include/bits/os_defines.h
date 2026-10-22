// Specific definitions for Darwin -*- C++ -*-

// Copyright (C) 2004 Free Software Foundation, Inc.
//
// This file is part of the GNU ISO C++ Library.  This library is free
// software; you can redistribute it and/or modify it under the
// terms of the GNU General Public License as published by the
// Free Software Foundation; either version 2, or (at your option)
// any later version.

// This library is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License along
// with this library; see the file COPYING.  If not, write to the Free
// Software Foundation, 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301,
// USA.

// As a special exception, you may use this file as part of a free software
// library without restriction.  Specifically, if other files instantiate
// templates or use macros or inline functions from this file, or you compile
// this file and link it with other files to produce an executable, this
// file does not by itself cause the resulting executable to be covered by
// the GNU General Public License.  This exception does not however
// invalidate any other reasons why the executable file might be covered by
// the GNU General Public License.

// GAME BRIDGE (2026-10-21): built for Linux/Android as the private C++
// runtime of iPhone games. Removed Apple's dtrace probes, <Availability.h>
// and the PowerPC keymgr declarations; the library is the iPhone OS
// flavour (__TARGETING_4_0_DYLIB is 0, as on iOS). Everything is hidden:
// operator new/delete here must never replace the host's.

#ifndef _GLIBCXX_OS_DEFINES
#define _GLIBCXX_OS_DEFINES 1

// System-specific #define, typedefs, corrections, etc, go here.  This
// file will come before all others.

/* pthread lives in the host C library; no weak references needed.  */
#define _GLIBCXX_GTHREAD_USE_WEAK 0

#define _GLIBCXX_WEAK_DEFINITION

#ifndef _GLIBCXX_VISIBILITY_DEFAULT
#define _GLIBCXX_VISIBILITY_DEFAULT /* _GLIBCXX_VISIBILITY(default) */
#endif

#ifndef __GNUC_LIBSTD__
#define __GNUC_LIBSTD__ 4
#define __GNUC_LIBSTD_MINOR__ 2
#endif

#define __TARGETING_4_0_DYLIB 0

#endif
