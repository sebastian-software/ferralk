# Changelog

## [0.11.0](https://github.com/sebastian-software/ferralk/compare/v0.10.0...v0.11.0) (2026-09-02)


### ⚠ BREAKING CHANGES

* **glob:** parse star runs before extglobs ([#324](https://github.com/sebastian-software/ferralk/issues/324))
* **glob:** align path entry point semantics ([#323](https://github.com/sebastian-software/ferralk/issues/323))
* **glob:** Without match_hidden, ordinary stars no longer match a hidden component by stopping immediately before its leading period (for example, *.rs no longer matches .rs).
* **walker:** close native traversal residuals ([#321](https://github.com/sebastian-software/ferralk/issues/321))
* **walker:** Relative roots with trailing separators or parent components now apply anchored repository ignore rules like their equivalent normalized filesystem roots, which can change emitted entries.
* **glob:** PatternError offsets from brace-expanded and absolute walker patterns now identify the original input bytes instead of intermediate rewritten patterns.
* **glob:** Case-insensitive POSIX upper classes now match ASCII letters instead of matching none, which can change glob and walker results.
* **walker:** Git-ignore matching now follows Git 2.52 for attached star runs, escaped separators, and reversed ranges, which can change which entries a walk includes.

### Bug Fixes

* **fuzz:** prevent matcher OOM regressions ([#326](https://github.com/sebastian-software/ferralk/issues/326)) ([2edeabd](https://github.com/sebastian-software/ferralk/commit/2edeabd608320b9b25d537524e4ee06b243fe6d0))
* **glob:** align path entry point semantics ([#323](https://github.com/sebastian-software/ferralk/issues/323)) ([fe71ce8](https://github.com/sebastian-software/ferralk/commit/fe71ce80857c6a3c30b5669d2c068bb87bd45675))
* **glob:** fold POSIX upper class symmetrically ([#318](https://github.com/sebastian-software/ferralk/issues/318)) ([638f369](https://github.com/sebastian-software/ferralk/commit/638f369a39616dd1c878a06c6c3ad76b2982c813)), closes [#299](https://github.com/sebastian-software/ferralk/issues/299)
* **glob:** parse star runs before extglobs ([#324](https://github.com/sebastian-software/ferralk/issues/324)) ([08e95df](https://github.com/sebastian-software/ferralk/commit/08e95dff940b722c24d257f5a0f8c2f20d77cd37))
* **glob:** preserve caller pattern error offsets ([#319](https://github.com/sebastian-software/ferralk/issues/319)) ([d39b9f5](https://github.com/sebastian-software/ferralk/commit/d39b9f596a5af5702077f06bb985103eadb83c90)), closes [#300](https://github.com/sebastian-software/ferralk/issues/300)
* **glob:** restore strict hidden-star pruning ([#322](https://github.com/sebastian-software/ferralk/issues/322)) ([29eca65](https://github.com/sebastian-software/ferralk/commit/29eca65d30ac027e1892ec655cffb9f11be22845)), closes [#298](https://github.com/sebastian-software/ferralk/issues/298)
* **walker:** align ignore wildmatch with Git 2.52 ([#316](https://github.com/sebastian-software/ferralk/issues/316)) ([f210f10](https://github.com/sebastian-software/ferralk/commit/f210f102dfa3f246d0067e8f409577bf4c473813)), closes [#311](https://github.com/sebastian-software/ferralk/issues/311)
* **walker:** close native traversal residuals ([#321](https://github.com/sebastian-software/ferralk/issues/321)) ([bd267c2](https://github.com/sebastian-software/ferralk/commit/bd267c28f3459a4fde2e6aa964711cdc03db8335))
* **walker:** normalize relative ignore roots ([#320](https://github.com/sebastian-software/ferralk/issues/320)) ([9b59191](https://github.com/sebastian-software/ferralk/commit/9b59191acdd0d16304e4261cb0a29b9fc04861e9))

## [0.10.0](https://github.com/sebastian-software/ferralk/compare/v0.9.4...v0.10.0) (2026-09-01)


### ⚠ BREAKING CHANGES

* **walker:** Git ignore rules now treat escaped slashes as separators, preserve matchable bracket-range bytes around slash endpoints, and no longer fuse literals across a special star-run separator.
* **walker:** Linux native walks now stop at the portable PATH_MAX boundary with ENAMETOOLONG instead of exposing deeper paths based on frontend scheduling or the process descriptor limit.
* **walker:** Walks rooted at ., ./, or a single relative component now inherit repository ignore rules exactly like equivalent absolute and multi-component roots; those rules could previously be skipped.

### Bug Fixes

* preserve hidden matches beneath excludes ([f66e481](https://github.com/sebastian-software/ferralk/commit/f66e4818fbf86abb12b467d82f557f592203f2af))
* preserve hidden matches beneath excludes ([b4fa811](https://github.com/sebastian-software/ferralk/commit/b4fa81142835dd1d083dadfe189c2b120ce89a25))
* restore the frozen zlob oracle ([a755d83](https://github.com/sebastian-software/ferralk/commit/a755d830e2916f1fe942264a34d9bf54593157ea))
* restore the frozen zlob oracle ([2be07f5](https://github.com/sebastian-software/ferralk/commit/2be07f54981f2fda2f34b5d8c6e37c4013216ffc))
* skip excluded symlink loops ([7a783aa](https://github.com/sebastian-software/ferralk/commit/7a783aa5429945a11b486b607abd4d20388acf0a))
* skip excluded symlink loops ([91ed805](https://github.com/sebastian-software/ferralk/commit/91ed805d0d4d539620800ac97cc90be701d4ea05))
* **walker:** align slash wildmatch rules with Git ([#295](https://github.com/sebastian-software/ferralk/issues/295)) ([7917410](https://github.com/sebastian-software/ferralk/commit/7917410b74e93fb2e0092b7d446fa8abaa62a41c))
* **walker:** bound Linux native paths ([5817730](https://github.com/sebastian-software/ferralk/commit/581773019c172b568c9ff450f360a116d9e1e5a4))
* **walker:** inherit ignores for relative roots ([1ec54c9](https://github.com/sebastian-software/ferralk/commit/1ec54c9c27c461a67bfc0b10b66e13f4d7e3e1fa))

## [0.9.4](https://github.com/sebastian-software/ferralk/compare/v0.9.3...v0.9.4) (2026-09-01)


### Bug Fixes

* align suffix star ignore rules with Git ([#263](https://github.com/sebastian-software/ferralk/issues/263)) ([4dc972b](https://github.com/sebastian-software/ferralk/commit/4dc972bcaa3a8e24e218e3e3fce008189663d6d2))
* **deps:** update rust crate fast-glob to v1.1.1 ([#243](https://github.com/sebastian-software/ferralk/issues/243)) ([e07a260](https://github.com/sebastian-software/ferralk/commit/e07a260f34a8fe4721982fc4ed9a83dd47f8c8bd))
* **deps:** update rust crate zlob to v1.6.5 ([#244](https://github.com/sebastian-software/ferralk/issues/244)) ([a1281a2](https://github.com/sebastian-software/ferralk/commit/a1281a26c46b76be812d74652209d113846c59e5))
* **glob:** reuse wide extglob sweep scratch ([#270](https://github.com/sebastian-software/ferralk/issues/270)) ([ac75a25](https://github.com/sebastian-software/ferralk/commit/ac75a25ec5ae5997df5e670cc7ab228f688c3af7))
* make scheduler wakeups observable ([#241](https://github.com/sebastian-software/ferralk/issues/241)) ([605ba99](https://github.com/sebastian-software/ferralk/commit/605ba99091d9ad69c1361bf95082c974f31a7503))
* preserve ignore anchors for dotted roots ([#264](https://github.com/sebastian-software/ferralk/issues/264)) ([573746a](https://github.com/sebastian-software/ferralk/commit/573746add6d24bc143ee2d14cb44c3baddfb1d72))
* preserve PATH_MAX entry parity on macOS ([#266](https://github.com/sebastian-software/ferralk/issues/266)) ([ac74c5f](https://github.com/sebastian-software/ferralk/commit/ac74c5f9c3466e571c14b2d3051b17ec24953a71))
* re-admit includes through symlink aliases ([#265](https://github.com/sebastian-software/ferralk/issues/265)) ([8914c7a](https://github.com/sebastian-software/ferralk/commit/8914c7a244282ca40d5bce0730c85d09aba40bd0))
* restore covering exclude pruning ([#268](https://github.com/sebastian-software/ferralk/issues/268)) ([c74220d](https://github.com/sebastian-software/ferralk/commit/c74220db085c5f6699956d6ca5036f732077d27c))
* restore serial directory emission order ([#267](https://github.com/sebastian-software/ferralk/issues/267)) ([6278819](https://github.com/sebastian-software/ferralk/commit/62788199faa93eaa0b57aeff0cae4542d6bf7672))


### Performance Improvements

* measure include and exclude pruning ([#269](https://github.com/sebastian-software/ferralk/issues/269)) ([0a412b3](https://github.com/sebastian-software/ferralk/commit/0a412b3b0dd0296ecac83ddfaad0e30d867715ae))
* **native-linux:** open children relative to parents ([#273](https://github.com/sebastian-software/ferralk/issues/273)) ([c7a6b2a](https://github.com/sebastian-software/ferralk/commit/c7a6b2a2eec969371c4a19e5a50f1d04dbdca4a9))
* **walker:** gate scheduler wakeups on sleepers ([#271](https://github.com/sebastian-software/ferralk/issues/271)) ([1463097](https://github.com/sebastian-software/ferralk/commit/1463097c1c15a0a97c908c9b0d404a30e647270f))

## [0.9.3](https://github.com/sebastian-software/ferralk/compare/v0.9.2...v0.9.3) (2026-08-31)


### ⚠ BREAKING CHANGES

* **walker:** followed symlinks now traverse every acyclic alias instead of
  suppressing later aliases to the same target ([#226](https://github.com/sebastian-software/ferralk/issues/226))
* **walker:** includes can now re-admit descendants below an excluded directory
  instead of the excluded parent always pruning the subtree ([#227](https://github.com/sebastian-software/ferralk/issues/227))


### Bug Fixes

* allow includes below excluded directories ([#227](https://github.com/sebastian-software/ferralk/issues/227)) ([3d2ce6c](https://github.com/sebastian-software/ferralk/commit/3d2ce6cad8a5597119ab9be6a7c8e7cb58e9f330))
* bound macOS directory entry records ([#236](https://github.com/sebastian-software/ferralk/issues/236)) ([1d6a745](https://github.com/sebastian-software/ferralk/commit/1d6a74553eb0aa2f2ab1f537f99aa24ff295387b))
* classify unknown native entries by descriptor ([#237](https://github.com/sebastian-software/ferralk/issues/237)) ([32d8cd2](https://github.com/sebastian-software/ferralk/commit/32d8cd2c0b2fa0756f4a940fc273c205aebcff1b))
* deduplicate queued extglob continuations ([#238](https://github.com/sebastian-software/ferralk/issues/238)) ([d5eb4ac](https://github.com/sebastian-software/ferralk/commit/d5eb4acf6b6a62ff39036dda6f3a6e800b6051e0))
* filter followed directory symlinks by target kind ([#230](https://github.com/sebastian-software/ferralk/issues/230)) ([41e27fe](https://github.com/sebastian-software/ferralk/commit/41e27fe695bda55c8386e51c1d8386af6a417f17))
* inherit repository ignores for subtree walks ([#228](https://github.com/sebastian-software/ferralk/issues/228)) ([2dd8f71](https://github.com/sebastian-software/ferralk/commit/2dd8f715b028a698203a8e89a36eeb3369244eff))
* keep native macOS walks within PATH_MAX ([#232](https://github.com/sebastian-software/ferralk/issues/232)) ([23dd6c7](https://github.com/sebastian-software/ferralk/commit/23dd6c79d2583fe2f0fbf750616b64d5ac940be4))
* match Git suffix star runs in ignore rules ([#229](https://github.com/sebastian-software/ferralk/issues/229)) ([bbaa205](https://github.com/sebastian-software/ferralk/commit/bbaa205bbc5e3fc89685d4f277cbc45c811f2f05))
* preserve serial walking on deeply nested trees ([#225](https://github.com/sebastian-software/ferralk/issues/225)) ([0322cd5](https://github.com/sebastian-software/ferralk/commit/0322cd58d664ab470552f59a4bda42bc282ea395))
* rewrite Windows verbatim roots ([#235](https://github.com/sebastian-software/ferralk/issues/235)) ([472ee22](https://github.com/sebastian-software/ferralk/commit/472ee22325a9496ef7e6cc4f13f449572696b9ab))
* traverse acyclic symlink aliases ([#226](https://github.com/sebastian-software/ferralk/issues/226)) ([49bfb44](https://github.com/sebastian-software/ferralk/commit/49bfb441720095642c18b9120fc15a0393f59795))
* **walker:** continue config assignments after headers ([#222](https://github.com/sebastian-software/ferralk/issues/222)) ([4660fe9](https://github.com/sebastian-software/ferralk/commit/4660fe9cfe964cd5233429c8fe2ac4fcf8f89757))
* **walker:** use libc O_DIRECTORY on Linux ([#221](https://github.com/sebastian-software/ferralk/issues/221)) ([abd4a48](https://github.com/sebastian-software/ferralk/commit/abd4a4862ba49d6098c4a5f791b168e914c55918))
* widen while processing large directory listings ([#239](https://github.com/sebastian-software/ferralk/issues/239)) ([a8e8033](https://github.com/sebastian-software/ferralk/commit/a8e8033fe2310c4c86c8d588112fd708d8ede407))


### Performance Improvements

* reuse Windows path bytes while classifying ([#234](https://github.com/sebastian-software/ferralk/issues/234)) ([2b706d0](https://github.com/sebastian-software/ferralk/commit/2b706d0fb78b125b4b0af2a8bde0c6e1cddbb806))
* reuse retained extglob group scratch ([#231](https://github.com/sebastian-software/ferralk/issues/231)) ([77e8ef3](https://github.com/sebastian-software/ferralk/commit/77e8ef319e78ee364973c7c79a80852873694ed3))

## [0.9.2](https://github.com/sebastian-software/ferralk/compare/v0.9.1...v0.9.2) (2026-08-31)


### Bug Fixes

* **walker:** parse config assignments after headers ([#199](https://github.com/sebastian-software/ferralk/issues/199)) ([616e7fe](https://github.com/sebastian-software/ferralk/commit/616e7fe545f7f38f16d55ef48935b0a1943f1ad7))
* **walker:** remove duplicate read-batch error branch ([#196](https://github.com/sebastian-software/ferralk/issues/196)) ([3c6d1c0](https://github.com/sebastian-software/ferralk/commit/3c6d1c0eb45d1b888037c9a7b17417d19d9e1b3f))
* **walker:** scan config continuations linearly ([#200](https://github.com/sebastian-software/ferralk/issues/200)) ([e190af4](https://github.com/sebastian-software/ferralk/commit/e190af40e33dc69b97a038fdf5160b7458b9c002))

## [0.9.1](https://github.com/sebastian-software/ferralk/compare/v0.9.0...v0.9.1) (2026-08-28)


### Performance Improvements

* **glob:** accelerate brace suffix sets ([#183](https://github.com/sebastian-software/ferralk/issues/183)) ([c9727b8](https://github.com/sebastian-software/ferralk/commit/c9727b8af732887f739e99da3101fca4c79b9db2))
* **walker:** narrow the zlob gap on macOS ([#186](https://github.com/sebastian-software/ferralk/issues/186)) ([a9a2c48](https://github.com/sebastian-software/ferralk/commit/a9a2c482f36f26384fce710f56a063b50aca60c7))

## [0.9.0](https://github.com/sebastian-software/ferralk/compare/v0.8.1...v0.9.0) (2026-08-27)


### ⚠ BREAKING CHANGES

* harden matcher and walker resource limits ([#178](https://github.com/sebastian-software/ferralk/issues/178)):
  `Walker::threads` now clamps requests to `1..=256`. Ignore and repository
  metadata files are capped at 8 MiB and rule files at 100,000 rules; an
  unreadable or over-limit rule file now reaches the configured `ErrorPolicy`
  as a `read_ignore` failure (including the default `Collect` policy), instead
  of being silently skipped. Pattern compilation also enforces bounded
  expansion and compiled-program budgets, so large wildcard patterns that
  compiled in 0.8.x may now return `pattern compiles to too much`.

### Features

* **walker:** finish post-0.8 polish ([#175](https://github.com/sebastian-software/ferralk/issues/175)) ([a01a3a0](https://github.com/sebastian-software/ferralk/commit/a01a3a04be8b11fb167250368995a9b0f7440e10))


### Bug Fixes

* harden matcher and walker resource limits ([#178](https://github.com/sebastian-software/ferralk/issues/178)) ([494abc9](https://github.com/sebastian-software/ferralk/commit/494abc9727c87136c67b453f3e1771bcb77a8f80))


### Performance Improvements

* **glob:** accelerate common suffix patterns ([#180](https://github.com/sebastian-software/ferralk/issues/180)) ([a83ccf4](https://github.com/sebastian-software/ferralk/commit/a83ccf4eccda09c04d977eb5017d5c43b95b84ec))
* **glob:** reuse positive extglob scratch ([#179](https://github.com/sebastian-software/ferralk/issues/179)) ([f0f60e3](https://github.com/sebastian-software/ferralk/commit/f0f60e3649d65511266e871380308fdbe74aebaa))
* **glob:** vectorize short suffix checks on Apple Silicon ([#181](https://github.com/sebastian-software/ferralk/issues/181)) ([af84e55](https://github.com/sebastian-software/ferralk/commit/af84e5543a8c47a34dca27574b5eba3b431c94e1))

## [0.8.1](https://github.com/sebastian-software/ferralk/compare/v0.8.0...v0.8.1) (2026-08-27)


### Bug Fixes

* bound extglob memo memory and dot context ([#170](https://github.com/sebastian-software/ferralk/issues/170)) ([832642f](https://github.com/sebastian-software/ferralk/commit/832642faadeffcde933d3c96909058182de1c3ea))
* harden native walker fallbacks ([#169](https://github.com/sebastian-software/ferralk/issues/169)) ([feedb48](https://github.com/sebastian-software/ferralk/commit/feedb48200898cebeb64299a23d3446519d64249))
* parse repository config without UTF-8 gate ([#168](https://github.com/sebastian-software/ferralk/issues/168)) ([7867c48](https://github.com/sebastian-software/ferralk/commit/7867c4861a915344c104d7d5a453b689c1f0f7a3))
* restore portable walker hot path and isolate Git tests ([#166](https://github.com/sebastian-software/ferralk/issues/166)) ([c95e5dc](https://github.com/sebastian-software/ferralk/commit/c95e5dca1490cea5734f1984cd1c2a3308b01162))

## [0.8.0](https://github.com/sebastian-software/ferralk/compare/v0.7.0...v0.8.0) (2026-08-27)


### Features

* add non-consuming walker pattern builders ([#153](https://github.com/sebastian-software/ferralk/issues/153)) ([e5b7534](https://github.com/sebastian-software/ferralk/commit/e5b7534016d556b9aaba3ad9bf9751341e5a9900))
* document crates and maintain MSRV policy ([#151](https://github.com/sebastian-software/ferralk/issues/151)) ([15a3df2](https://github.com/sebastian-software/ferralk/commit/15a3df278b0ddcf4121c29878b9f0abdc36685d9))


### Bug Fixes

* align Git ignore filesystem adaptations ([#152](https://github.com/sebastian-software/ferralk/issues/152)) ([bf850e6](https://github.com/sebastian-software/ferralk/commit/bf850e6a25dbf79b56a0d70907b9126d230cb78d))
* clarify walker root semantics ([#148](https://github.com/sebastian-software/ferralk/issues/148)) ([81d7ac2](https://github.com/sebastian-software/ferralk/commit/81d7ac2328a29e9c5607c73c9a0ea26750da3574))
* scope symlink cycle guards per root ([#150](https://github.com/sebastian-software/ferralk/issues/150)) ([d609d53](https://github.com/sebastian-software/ferralk/commit/d609d539a7186056c6805d80f93ab7a99a555a8d))


### Compatibility notes

* **Breaking:** pattern compilation now rejects `.` and `..` path components
  instead of accepting them. Correct the pattern before constructing a matcher
  or walker.
* `ErrorPolicy::Skip` now reports every failure for a caller-supplied root,
  including read, metadata, and canonicalize failures in follow-symlink walks.
  It still skips recoverable failures discovered below a root.

## [0.7.0](https://github.com/sebastian-software/ferralk/compare/v0.6.1...v0.7.0) (2026-08-27)


### Features

* bridge walker paths to byte matchers ([#147](https://github.com/sebastian-software/ferralk/issues/147)) ([82e8dc7](https://github.com/sebastian-software/ferralk/commit/82e8dc79e2a3015bd8356b17137f18ab5800d724))


### Bug Fixes

* align Git ignore repository semantics ([#144](https://github.com/sebastian-software/ferralk/issues/144)) ([71e7622](https://github.com/sebastian-software/ferralk/commit/71e762223e5da08bfde35c8f25450d475f558eb0))
* align gitignore byte parsing and fuzz matching ([#143](https://github.com/sebastian-software/ferralk/issues/143)) ([6e9d83e](https://github.com/sebastian-software/ferralk/commit/6e9d83e680a3e529763c9e2e6ba383fb68a90a96))
* harden native directory backend contracts ([#146](https://github.com/sebastian-software/ferralk/issues/146)) ([6da4add](https://github.com/sebastian-software/ferralk/commit/6da4add1691d9b53cdd4979c1e3fc65ede7fa48f))
* isolate parallel walker shutdown ([#145](https://github.com/sebastian-software/ferralk/issues/145)) ([cdcb1c9](https://github.com/sebastian-software/ferralk/commit/cdcb1c9ed57bbae000ed1afee7f4a8b340493106))
* memoize extglob repetition states ([#141](https://github.com/sebastian-software/ferralk/issues/141)) ([068927c](https://github.com/sebastian-software/ferralk/commit/068927cb2ae2043f1769c817997756ec7fd529a2))


### Compatibility notes

* **Breaking:** walker-internal aborts, visitor stops, worker-start failures,
  and worker panics no longer cancel a caller-shared `CancellationToken`.
  Walkers only observe that token; callers retain ownership and may reuse it.

## [0.6.1](https://github.com/sebastian-software/ferralk/compare/v0.6.0...v0.6.1) (2026-08-26)


### Bug Fixes

* **deps:** pin dependencies ([#102](https://github.com/sebastian-software/ferralk/issues/102)) ([b9f73f0](https://github.com/sebastian-software/ferralk/commit/b9f73f0ad206c65347d443884bc0c9c5e35422bb))
* discard partial stream listings after read errors ([#138](https://github.com/sebastian-software/ferralk/issues/138)) ([b23872f](https://github.com/sebastian-software/ferralk/commit/b23872fba53a017aa915f6d81ecce5c28f231d96))
* handle literal-dot extglobs and malformed POSIX classes ([#137](https://github.com/sebastian-software/ferralk/issues/137)) ([8e78680](https://github.com/sebastian-software/ferralk/commit/8e78680ad92204797837dfa358bcedfeaa55bafc))

## [0.6.0](https://github.com/sebastian-software/ferralk/compare/v0.5.3...v0.6.0) (2026-08-25)


### Features

* **glob:** bit-parallel Shift-And engine for the general match path ([1b00ca8](https://github.com/sebastian-software/ferralk/commit/1b00ca8534ee5d7a04d302caff6fd74f9182d781))

## [0.5.3](https://github.com/sebastian-software/ferralk/compare/v0.5.2...v0.5.3) (2026-08-20)


### Performance Improvements

* getdirentries64 on macOS, a matcher prefilter, and a lighter walk hot path ([27400f0](https://github.com/sebastian-software/ferralk/commit/27400f09a82d6454af3b5fc3dc11767c2c4cc3c6))
* **glob:** reject on a pattern's fixed ends before the general engine ([34be594](https://github.com/sebastian-software/ferralk/commit/34be5949d70e908b9a1d388cd341be22a4c61fcd))
* **walker:** read macOS directories with getdirentries64 ([286c7c5](https://github.com/sebastian-software/ferralk/commit/286c7c5db35ae38477032da117fe32a169517b90))
* **walker:** trim the per-entry and per-directory hot path ([d0bc87e](https://github.com/sebastian-software/ferralk/commit/d0bc87ea783ca1fc11b23334b883092913679f27))

## [0.5.2](https://github.com/sebastian-software/ferralk/compare/v0.5.1...v0.5.2) (2026-08-20)


### Bug Fixes

* **walker:** read the path-shaped check outside groups only ([2781961](https://github.com/sebastian-software/ferralk/commit/2781961226db021aa86d40c6b8936a3f1af38556))
* **walker:** refuse Windows paths handed over as patterns ([c70745e](https://github.com/sebastian-software/ferralk/commit/c70745e9ec5fddee08789d3d206baddc67ff1a1b))
* **walker:** refuse Windows paths handed over as patterns ([dd64074](https://github.com/sebastian-software/ferralk/commit/dd64074ad6b2af2fa3863d3a0b8fc2430699a1e1)), closes [#94](https://github.com/sebastian-software/ferralk/issues/94)

## [0.5.1](https://github.com/sebastian-software/ferralk/compare/v0.5.0...v0.5.1) (2026-08-20)


### Performance Improvements

* **walker:** weigh a directory listing in the helper floor ([cfee83d](https://github.com/sebastian-software/ferralk/commit/cfee83d41d99675a04f365a2f4929bdebb2bfac6))

## [0.5.0](https://github.com/sebastian-software/ferralk/compare/v0.4.0...v0.5.0) (2026-08-20)


### Features

* **walker:** classify symlink entries by their target on request ([e5027de](https://github.com/sebastian-software/ferralk/commit/e5027de3cafe80954c82d3b6e7742d099e40016e)), closes [#89](https://github.com/sebastian-software/ferralk/issues/89)

## [0.4.0](https://github.com/sebastian-software/ferralk/compare/v0.3.0...v0.4.0) (2026-08-20)


### Features

* **walker:** reach separator-crossing wildcards from the builder ([380dc7f](https://github.com/sebastian-software/ferralk/commit/380dc7f102f88c18d88e4c633778c8c8a5feae6e)), closes [#79](https://github.com/sebastian-software/ferralk/issues/79)
* **walker:** rewrite absolute include and exclude patterns ([6315498](https://github.com/sebastian-software/ferralk/commit/6315498ab97c4ccdbaa1ebb0706862426b2c31b0)), closes [#78](https://github.com/sebastian-software/ferralk/issues/78)
* **walker:** walk several roots with one walker and one pool ([9873c63](https://github.com/sebastian-software/ferralk/commit/9873c63cd7bcbc18b9cd5275b4dfe9de6521f4dc))


### Bug Fixes

* **walker:** build the linux native backend against multi-root tasks ([e9c7e01](https://github.com/sebastian-software/ferralk/commit/e9c7e013662954cb03da9427fb45b321e417fadd))
* **walker:** prune a subtree only where its exclude reaches ([727e963](https://github.com/sebastian-software/ferralk/commit/727e963685a86cb6562f5ece5bc57a47c7f5976f))


### Performance Improvements

* **walker:** build the pool only once the floor unlocks a helper ([3e6547f](https://github.com/sebastian-software/ferralk/commit/3e6547f82c7d90c197e9ba017b66c4c6fa54c7cc)), closes [#81](https://github.com/sebastian-software/ferralk/issues/81)
* **walker:** hand entries to the visitor without materializing them ([bbfdfd8](https://github.com/sebastian-software/ferralk/commit/bbfdfd88ba76e560b6144e467a8134218efdcaef)), closes [#73](https://github.com/sebastian-software/ferralk/issues/73)
* **walker:** weigh work, not directories, before starting a pool ([0d192f0](https://github.com/sebastian-software/ferralk/commit/0d192f0966a0a23b48f7e4c68dc4d3c119e2666b)), closes [#76](https://github.com/sebastian-software/ferralk/issues/76)

## [0.3.0](https://github.com/sebastian-software/ferralk/compare/v0.2.0...v0.3.0) (2026-08-20)


### Features

* **walker:** expose match_hidden on the Walker builder ([c833f54](https://github.com/sebastian-software/ferralk/commit/c833f54a20080245aadaef69c5674276b17e666d)), closes [#63](https://github.com/sebastian-software/ferralk/issues/63)
* **walker:** filter entries on the workers that produced them ([851d17b](https://github.com/sebastian-software/ferralk/commit/851d17bba664aa88a1ebbbbc8a93e5bba1bcbee0)), closes [#64](https://github.com/sebastian-software/ferralk/issues/64)
* **walker:** own gitignore rule matching ([068a144](https://github.com/sebastian-software/ferralk/commit/068a144ebc7801959c10afb35f6fb2beefb61b42)), closes [#49](https://github.com/sebastian-software/ferralk/issues/49)


### Bug Fixes

* **glob:** bound how much program a pattern compiles to ([1e47041](https://github.com/sebastian-software/ferralk/commit/1e4704195fd54eedf28e7190446c3b6c118e8838)), closes [#74](https://github.com/sebastian-software/ferralk/issues/74)


### Performance Improvements

* **walker:** match ignore rules without recursion ([9413aad](https://github.com/sebastian-software/ferralk/commit/9413aad509e91a72628d2a16f12a4de1488dcd63))

## [0.2.0](https://github.com/sebastian-software/ferralk/compare/v0.1.2...v0.2.0) (2026-08-20)


### Features

* **glob:** expose brace expansion ([898ca7d](https://github.com/sebastian-software/ferralk/commit/898ca7df5289c2068dd9c9bf3717777ddc06bfae))


### Bug Fixes

* **fuzz:** construct corpus cases with the new ignore files field ([88472ff](https://github.com/sebastian-software/ferralk/commit/88472ff8f94e3e5458f8717b5ec9beabe1be78cd))
* **glob:** bound brace expansion and drop its recursion ([f8d8311](https://github.com/sebastian-software/ferralk/commit/f8d8311c7c5520c343c81e63e993d6b9a5a9d0f4)), closes [#42](https://github.com/sebastian-software/ferralk/issues/42)
* **glob:** bound the work brace expansion may do ([e7ef63e](https://github.com/sebastian-software/ferralk/commit/e7ef63e7e6f17734eae139d1b08ed10e44ae23e2)), closes [#54](https://github.com/sebastian-software/ferralk/issues/54)
* **glob:** drive star repetition without native recursion ([7008f01](https://github.com/sebastian-software/ferralk/commit/7008f012ab89b07a81dbf2e5abaa7b2c38c4060e)), closes [#17](https://github.com/sebastian-software/ferralk/issues/17)
* **glob:** keep an escaped dash literal inside a character class ([44b364c](https://github.com/sebastian-software/ferralk/commit/44b364cf7c0efde6b3f93f42d28e9355061a7497)), closes [#16](https://github.com/sebastian-software/ferralk/issues/16)
* **oracle:** skip cases zlob cannot represent instead of panicking ([9dcd883](https://github.com/sebastian-software/ferralk/commit/9dcd883f7e0ef7d1c71497eccddcde82a162a31c))
* **walker:** build the native backend tests again ([c6363fb](https://github.com/sebastian-software/ferralk/commit/c6363fb9f968ec7c88b24dafabc1df7c4c4665f5))
* **walker:** classify entries in one place ([48ad846](https://github.com/sebastian-software/ferralk/commit/48ad846b16e4230030c6320a10fc87a45c539125)), closes [#21](https://github.com/sebastian-software/ferralk/issues/21)
* **walker:** close the scheduler wakeup races ([3fe39a8](https://github.com/sebastian-software/ferralk/commit/3fe39a81a6d82392dd6cbc07c530844cfa489050)), closes [#24](https://github.com/sebastian-software/ferralk/issues/24)
* **walker:** degrade native directory reads per entry, not per directory ([081d64e](https://github.com/sebastian-software/ferralk/commit/081d64ecb9e7f090cfa27c96d7eb09ec9622c916))
* **walker:** native backend fallback robustness ([a10a563](https://github.com/sebastian-software/ferralk/commit/a10a563e64ed01d605c980bde93dc8c72bc93bba))
* **walker:** release panicked worker tasks ([21a0f7a](https://github.com/sebastian-software/ferralk/commit/21a0f7ae54a7ad316d8eea6d21a5e7309a6c9c74)), closes [#22](https://github.com/sebastian-software/ferralk/issues/22)


### Performance Improvements

* **glob:** compile extglob groups instead of interpreting pattern bytes ([a7b9538](https://github.com/sebastian-software/ferralk/commit/a7b9538df8f6163f2318e5b0e2a659d8639cd1b2)), closes [#15](https://github.com/sebastian-software/ferralk/issues/15)
* **glob:** reuse the matcher scratch and skip to the next literal ([0ccd4bd](https://github.com/sebastian-software/ferralk/commit/0ccd4bd06f41471ebd0593a4b460578cce44371f)), closes [#18](https://github.com/sebastian-software/ferralk/issues/18)
* **glob:** scan short candidates without memchr entry cost ([5ea450d](https://github.com/sebastian-software/ferralk/commit/5ea450ddba1d1d99c5e562c40260d2b7868f9487))
* **glob:** stop skipping when the literal is dense ([8e5195f](https://github.com/sebastian-software/ferralk/commit/8e5195f4abb7705c10547ad1ffad94dddc065c4a))
* **walker:** evaluate ignore rules per directory ([df16af5](https://github.com/sebastian-software/ferralk/commit/df16af57175e2ebfe72097785f79e5d95f6c1aac)), closes [#19](https://github.com/sebastian-software/ferralk/issues/19)
* **walker:** keep the prefilters on brace patterns ([06a4288](https://github.com/sebastian-software/ferralk/commit/06a428887eb78289d5b806de10052dabf89d8f92)), closes [#20](https://github.com/sebastian-software/ferralk/issues/20)
* **walker:** key the follow-symlinks guard on dev and inode ([c365c87](https://github.com/sebastian-software/ferralk/commit/c365c87d47ff9c5a191b90580aa7bff1f5107865))
* **walker:** widen the walk below the root ([e19a4fe](https://github.com/sebastian-software/ferralk/commit/e19a4fe8f386ed67e4b4f09ca54ef36f238394ce)), closes [#23](https://github.com/sebastian-software/ferralk/issues/23)

## [0.1.2](https://github.com/sebastian-software/ferralk/compare/v0.1.1...v0.1.2) (2026-08-19)


### Bug Fixes

* **release:** recover release please baseline ([ddff26e](https://github.com/sebastian-software/ferralk/commit/ddff26efc1f1f555c5a6ff0146cc7ba28b6ba73b))

## [0.1.1](https://github.com/sebastian-software/ferralk/compare/v0.1.0...v0.1.1) (2026-08-19)


### Features

* add base-relative path filtering ([d2b027a](https://github.com/sebastian-software/ferralk/commit/d2b027ab7cb3005335dbfcfbd113d85c35145149))
* add byte-first matcher baseline ([50c5d6b](https://github.com/sebastian-software/ferralk/commit/50c5d6be0cb85d72484442bc0904cef105945f39))
* add cooperative walker cancellation ([8109b99](https://github.com/sebastian-software/ferralk/commit/8109b99723db34ec4042f1c1246703ae52a0e1c0))
* add directory-only walker filtering ([1009275](https://github.com/sebastian-software/ferralk/commit/1009275aeadf58a437d41d2f9d5caeb456928e3b))
* add hidden walker filtering ([26cb7df](https://github.com/sebastian-software/ferralk/commit/26cb7dfd3b9232ea9393ff3539f8d6eba968d17a))
* add incremental walker stream ([99cedde](https://github.com/sebastian-software/ferralk/commit/99cedde5025d6afd463ed10023a3a3b4baabb8d2))
* add lazy parallel walker collection ([85157d0](https://github.com/sebastian-software/ferralk/commit/85157d05f887ae428a71a636550c672d8e7ba326))
* add linux native directory backend ([c93a6dc](https://github.com/sebastian-software/ferralk/commit/c93a6dc7a972410f5501d4562c2877e5ea1ce20f))
* add macos native directory backend ([b65c143](https://github.com/sebastian-software/ferralk/commit/b65c143cd212c648acfbfb18b8a548ef9e49b709))
* add optional walker metadata collection ([8fabb4d](https://github.com/sebastian-software/ferralk/commit/8fabb4d64270221be68cec918261a84ba77c6af7))
* add path filter index operations ([0cacba8](https://github.com/sebastian-software/ferralk/commit/0cacba85865a8ca72906b15babadf61e99794af0))
* add path-list corpus operations ([797f38c](https://github.com/sebastian-software/ferralk/commit/797f38c9d2b1cd2f938c05d5f37cd410683ff5e7))
* add portable serial walker baseline ([c443488](https://github.com/sebastian-software/ferralk/commit/c44348832e908cebee96bb882483e26d8c73d4b1))
* add POSIX character classes ([18ad949](https://github.com/sebastian-software/ferralk/commit/18ad949ea815f55f7aad632c0b3f2c5038b3f369))
* add scheduler queue foundation ([6a0104e](https://github.com/sebastian-software/ferralk/commit/6a0104ea38f6412b85c5930082578988f25eafad))
* add wildcard syntax preflight ([bb4a6e4](https://github.com/sebastian-software/ferralk/commit/bb4a6e4eb2417c501c99d387dc6e813b466d9827))
* add zlob-compatible extglob matcher ([9e98817](https://github.com/sebastian-software/ferralk/commit/9e98817f447e21011bd9acbc7ceb095a4b433641))
* apply root gitignore rules in walker ([1ff7586](https://github.com/sebastian-software/ferralk/commit/1ff75866e672af95e69f606888ce8a692015e6dd))
* batch macos directory attributes ([5152dac](https://github.com/sebastian-software/ferralk/commit/5152daca4b1073630d43aa21242c058ead74fbba))
* establish M0 workspace and corpus foundation ([4e27966](https://github.com/sebastian-software/ferralk/commit/4e27966a00f548971a4258f27d9187aac05469f8))
* evaluate nested gitignore rules ([17a0b39](https://github.com/sebastian-software/ferralk/commit/17a0b399abec3c67dc817fd15076ff714cae02e3))
* expose walker entry basenames ([c25e0f2](https://github.com/sebastian-software/ferralk/commit/c25e0f28f3f31ea47efd933ef34f12d0a2f5bedb))
* expose walker entry depth ([d522d62](https://github.com/sebastian-software/ferralk/commit/d522d62a3c053f5056c8dd65f999d3e47a202471))
* expose walker entry kinds ([e00921b](https://github.com/sebastian-software/ferralk/commit/e00921bfaf912545d136bacb55bb75c98e6647c5))
* extend walker depth and ignore corpus ([88f8af3](https://github.com/sebastian-software/ferralk/commit/88f8af3bc2ba33ef93ab27d06265bfa4ad23f47a))
* filter directory walk results ([aeda5c0](https://github.com/sebastian-software/ferralk/commit/aeda5c0e62324fcced06f2ad4012204773a4e4ab))
* handle git directories in ignored walks ([c879e82](https://github.com/sebastian-software/ferralk/commit/c879e82a2fcef4208f359446576394f400306836))
* initial rfc dump ([adfbc04](https://github.com/sebastian-software/ferralk/commit/adfbc0435823aad07dcde7b46ce28d25bc2806ca))
* load zlob ignore supplements ([97cd132](https://github.com/sebastian-software/ferralk/commit/97cd132f11047e9dff6c8cca447bdc7e5fb1bca8))
* normalize dot slash path filters ([20bc1d2](https://github.com/sebastian-software/ferralk/commit/20bc1d22575a0cc0e6ba8bcd855ffe54a70410c9))
* prune explicit excluded subtrees ([16460da](https://github.com/sebastian-software/ferralk/commit/16460da6579d3af6c760744eb7dbb49a03dabb88))
* share nested gitignore parent chains ([6cd4387](https://github.com/sebastian-software/ferralk/commit/6cd438787cb7dbcc92e0b95dfe3cd2d51f56b30a))
* support directory-only walker patterns ([a9ae5ce](https://github.com/sebastian-software/ferralk/commit/a9ae5ce314f1fa29a692e59a2b3d701703f5b914))
* support nested brace alternatives ([f2d5535](https://github.com/sebastian-software/ferralk/commit/f2d5535c5747145e99fedb93bd277769d866c404))


### Bug Fixes

* align core matcher with zlob semantics ([17a95d8](https://github.com/sebastian-software/ferralk/commit/17a95d88f23ceeac1e40102a5e7274d845da760d))
* constrain nonrecursive double stars in path filters ([2710557](https://github.com/sebastian-software/ferralk/commit/2710557458d8ee4bafe55a33afb1d8674ae5ff4d))
* enforce component boundaries in walker includes ([4a7bfaf](https://github.com/sebastian-software/ferralk/commit/4a7bfaf7f477d9c4b555c139d789632e9480c8da))
* honor hidden policy for empty extglobs ([38be2c3](https://github.com/sebastian-software/ferralk/commit/38be2c3cc48a2bd7ac23a10f8e79a08abbe8b038))
* include terminal recursive globstar roots ([5e3c6bf](https://github.com/sebastian-software/ferralk/commit/5e3c6bf56372b28ea0181600a3d54afa5679b1f8))
* load Release Please workspace config ([64ea535](https://github.com/sebastian-software/ferralk/commit/64ea535ea743f5294fde115cc7f06f75c5aba56b))
* normalize parallel walker paths on Windows ([160dc82](https://github.com/sebastian-software/ferralk/commit/160dc82931ddb90c1ba39af219451deb789456a9))
* parse Linux getdents64 records correctly ([a6dfc23](https://github.com/sebastian-software/ferralk/commit/a6dfc23c242f2e52e1741a070c55b8b1ece29aa7))
* preserve gitignore negation re-includes ([1d62601](https://github.com/sebastian-software/ferralk/commit/1d626019f62caeb3cda13281209efe76bff364fa))
* preserve literal include prefix semantics ([1f92b6e](https://github.com/sebastian-software/ferralk/commit/1f92b6e059910e10de78461098c9e948d058692e))
* preserve walker component boundaries in extglobs ([791bc0f](https://github.com/sebastian-software/ferralk/commit/791bc0febbb531fff7ae34ef08a81081061c740c))
* restore cross-platform CI validation ([d69c597](https://github.com/sebastian-software/ferralk/commit/d69c597bc6a68188aaccb5a58dea6f38a8c35e73))
* restore Rust 1.93 compatibility ([854016b](https://github.com/sebastian-software/ferralk/commit/854016be45cd00b5b8f42b37a79746acf32fc98e))
* restrict walker root wildcards to components ([3098437](https://github.com/sebastian-software/ferralk/commit/3098437279835757b5e04f52e61e9679c9fdc6d3))
* satisfy benchmark workspace lint ([a1f494c](https://github.com/sebastian-software/ferralk/commit/a1f494c337d3f5e2233642d1adb84f304a657f40))
* scope path-list wildcards to components ([ae34aec](https://github.com/sebastian-software/ferralk/commit/ae34aec8509424ff229633df0b6d34840bd52731))
* unify parallel walker cancellation ([67f54c5](https://github.com/sebastian-software/ferralk/commit/67f54c5bf8995b49c9ac153b5f91946952a9316e))


### Performance Improvements

* accelerate glob byte scans ([db478ca](https://github.com/sebastian-software/ferralk/commit/db478ca2db3673b5a759efc06dbbce9adf10cfc4))
* add deterministic matcher ir fast path ([3372e76](https://github.com/sebastian-software/ferralk/commit/3372e769164589552c2abd0ee75d732da4f2d083))
* broaden static star matcher ir ([3d0476c](https://github.com/sebastian-software/ferralk/commit/3d0476c3ab21b050386d749040e2fd68a8ee55ed))
* cache gitignore matchers per traversal ([570eda9](https://github.com/sebastian-software/ferralk/commit/570eda9fe9b7d9206ec55c38cf5b508e4665e5eb))
* dispatch single fast matcher alternatives directly ([eebe4e6](https://github.com/sebastian-software/ferralk/commit/eebe4e6e810ec38cd6c6ddcb9934acef12d064df))
* fast path literal glob patterns ([c1f4e82](https://github.com/sebastian-software/ferralk/commit/c1f4e820064408d9e6b7bdd89d89bad865bf6854))
* fold recursive matcher prefixes ([6274eb1](https://github.com/sebastian-software/ferralk/commit/6274eb16bef986c5c6e3c880b5b3e8fd1056d2f3))
* inline small matcher state memos ([5e476c6](https://github.com/sebastian-software/ferralk/commit/5e476c65bb63971a6d95ad6f81b531503ede12b3))
* precompile path filter ir ([144eb48](https://github.com/sebastian-software/ferralk/commit/144eb483444252908ac68f54f33f2fd07612090e))
* prefilter walker files by extension ([aaff8bd](https://github.com/sebastian-software/ferralk/commit/aaff8bd5f41f47dfc3b435b785641aaa9e22cf81))
* prune literal include roots ([d597a7d](https://github.com/sebastian-software/ferralk/commit/d597a7d37a56e3106d0383e538ef85ce5b3754bc))
* reject recursive suffix mismatches early ([b465494](https://github.com/sebastian-software/ferralk/commit/b4654945b8889c44b6a8ee18051c4aa707a094a5))
* reuse native directory buffers per thread ([b3c9d02](https://github.com/sebastian-software/ferralk/commit/b3c9d02967f42d42edc564d321f6ced5ca577dae))
* shard parallel walker results per worker ([7f55523](https://github.com/sebastian-software/ferralk/commit/7f55523f761166befadb28e0dc0a4a3d367c05f2))
* skip path filter clone for root patterns ([f2db8c7](https://github.com/sebastian-software/ferralk/commit/f2db8c7bcf2e0a4e90ed9943b8c803688fa74cca))
* specialize component deterministic matchers ([157db8b](https://github.com/sebastian-software/ferralk/commit/157db8be802a0e6469515a3f8a4d2fd3458a6a1c))
* specialize infix star matchers ([7baba15](https://github.com/sebastian-software/ferralk/commit/7baba1536606730c66e513aa9f6539281e4cae6d))
* specialize macos native file entries ([b3a973d](https://github.com/sebastian-software/ferralk/commit/b3a973d0103dbd1e87f21ef412a380cad72f6892))
* specialize recursive prefix suffix matches ([f03a591](https://github.com/sebastian-software/ferralk/commit/f03a59177ab05f3976cf28ee2655883d874623ce))
* specialize single-star glob matching ([c45a277](https://github.com/sebastian-software/ferralk/commit/c45a277bcf2a8f511b9fff45e95b0fb8e89ee975))
* specialize static single star matchers ([6a77358](https://github.com/sebastian-software/ferralk/commit/6a77358d99eafe73df7141748623e1b688032dad))
* specialize terminal recursive globstars ([f5178ec](https://github.com/sebastian-software/ferralk/commit/f5178ec265c4ebc09688ae6ead873701e0fdc1b2))
* streamline recursive matcher prefix ([aa0f38c](https://github.com/sebastian-software/ferralk/commit/aa0f38c0fbf1dea54a63037afed25102a49c8811))
* use dense matcher failure states ([f4a2d24](https://github.com/sebastian-software/ferralk/commit/f4a2d24c1e1d7f2cb9529d4e284cec0f01b12786))

## Changelog

All notable user-facing changes are recorded here. Release Please maintains
versioned sections below this introduction from conventional commits.
