// This file "links" a concrete implementation of the Atom instance to the procedure bridge
//
// The lodestone/dev URL below is not fetched from GitHub: Lodestone's module loader
// serves it from the copy of core/src/implementations/generic/js/main/mod.ts that
// is embedded in the binary (see core/src/embedded_glue.rs).

import * as a from "REPLACE_ME_WITH_URL";
import { run } from "https://raw.githubusercontent.com/Lodestone-Team/lodestone/dev/core/src/implementations/generic/js/main/mod.ts";
const instance = new a.default();
run(instance);