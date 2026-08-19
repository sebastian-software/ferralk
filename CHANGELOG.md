# Changelog

## [0.2.0](https://github.com/sebastian-software/ferralk/compare/v0.1.2...v0.2.0) (2026-08-19)


### Features

* **glob:** expose brace expansion ([898ca7d](https://github.com/sebastian-software/ferralk/commit/898ca7df5289c2068dd9c9bf3717777ddc06bfae))


### Bug Fixes

* **fuzz:** construct corpus cases with the new ignore files field ([88472ff](https://github.com/sebastian-software/ferralk/commit/88472ff8f94e3e5458f8717b5ec9beabe1be78cd))
* **glob:** bound brace expansion and drop its recursion ([b9505a7](https://github.com/sebastian-software/ferralk/commit/b9505a7263b785adf5f02b37992a03b53186f6ae))
* **glob:** bound brace expansion and drop its recursion ([f8d8311](https://github.com/sebastian-software/ferralk/commit/f8d8311c7c5520c343c81e63e993d6b9a5a9d0f4)), closes [#42](https://github.com/sebastian-software/ferralk/issues/42)
* **glob:** bound the work brace expansion may do ([0cc0f78](https://github.com/sebastian-software/ferralk/commit/0cc0f781f06d4858149f95b0a00c6e20c2bb5a57))
* **glob:** bound the work brace expansion may do ([e7ef63e](https://github.com/sebastian-software/ferralk/commit/e7ef63e7e6f17734eae139d1b08ed10e44ae23e2)), closes [#54](https://github.com/sebastian-software/ferralk/issues/54)
* **glob:** drive star repetition without native recursion ([3cb10d3](https://github.com/sebastian-software/ferralk/commit/3cb10d3f24ef4ae7c484e5aa6b6ba1fbe1b13f97))
* **glob:** drive star repetition without native recursion ([7008f01](https://github.com/sebastian-software/ferralk/commit/7008f012ab89b07a81dbf2e5abaa7b2c38c4060e)), closes [#17](https://github.com/sebastian-software/ferralk/issues/17)
* **glob:** keep an escaped dash literal inside a character class ([a883812](https://github.com/sebastian-software/ferralk/commit/a883812053a446b15b61719cfbec1050df44e2b1))
* **glob:** keep an escaped dash literal inside a character class ([44b364c](https://github.com/sebastian-software/ferralk/commit/44b364cf7c0efde6b3f93f42d28e9355061a7497)), closes [#16](https://github.com/sebastian-software/ferralk/issues/16)
* **oracle:** skip cases zlob cannot represent instead of panicking ([9dcd883](https://github.com/sebastian-software/ferralk/commit/9dcd883f7e0ef7d1c71497eccddcde82a162a31c))
* **walker:** build the native backend tests again ([c6363fb](https://github.com/sebastian-software/ferralk/commit/c6363fb9f968ec7c88b24dafabc1df7c4c4665f5))
* **walker:** classify entries in one place ([6d96e0c](https://github.com/sebastian-software/ferralk/commit/6d96e0c53304c6e398d2adf81960b4a651edc45d))
* **walker:** classify entries in one place ([48ad846](https://github.com/sebastian-software/ferralk/commit/48ad846b16e4230030c6320a10fc87a45c539125)), closes [#21](https://github.com/sebastian-software/ferralk/issues/21)
* **walker:** close the scheduler wakeup races ([929ff0d](https://github.com/sebastian-software/ferralk/commit/929ff0deb818e4b2f0fbf490b97976ddc93e4dd4))
* **walker:** close the scheduler wakeup races ([3fe39a8](https://github.com/sebastian-software/ferralk/commit/3fe39a81a6d82392dd6cbc07c530844cfa489050)), closes [#24](https://github.com/sebastian-software/ferralk/issues/24)
* **walker:** degrade native directory reads per entry, not per directory ([081d64e](https://github.com/sebastian-software/ferralk/commit/081d64ecb9e7f090cfa27c96d7eb09ec9622c916))
* **walker:** native backend fallback robustness ([a10a563](https://github.com/sebastian-software/ferralk/commit/a10a563e64ed01d605c980bde93dc8c72bc93bba))
* **walker:** release panicked worker tasks ([0c9a4ae](https://github.com/sebastian-software/ferralk/commit/0c9a4aedeabcecc231a9ae4fb067277a57b63eee))
* **walker:** release panicked worker tasks ([21a0f7a](https://github.com/sebastian-software/ferralk/commit/21a0f7ae54a7ad316d8eea6d21a5e7309a6c9c74)), closes [#22](https://github.com/sebastian-software/ferralk/issues/22)


### Performance Improvements

* **glob:** compile extglob groups instead of interpreting pattern bytes ([0c0ceae](https://github.com/sebastian-software/ferralk/commit/0c0ceae14c98aeb5e35e5fe616e1c5c7ac7f8bc2))
* **glob:** compile extglob groups instead of interpreting pattern bytes ([a7b9538](https://github.com/sebastian-software/ferralk/commit/a7b9538df8f6163f2318e5b0e2a659d8639cd1b2)), closes [#15](https://github.com/sebastian-software/ferralk/issues/15)
* **glob:** reuse the matcher scratch and skip to the next literal ([2e8809e](https://github.com/sebastian-software/ferralk/commit/2e8809ea6ae89a4b2c9af4f13c151f6d8c1dd056))
* **glob:** reuse the matcher scratch and skip to the next literal ([0ccd4bd](https://github.com/sebastian-software/ferralk/commit/0ccd4bd06f41471ebd0593a4b460578cce44371f)), closes [#18](https://github.com/sebastian-software/ferralk/issues/18)
* **glob:** scan short candidates without memchr entry cost ([5ea450d](https://github.com/sebastian-software/ferralk/commit/5ea450ddba1d1d99c5e562c40260d2b7868f9487))
* **glob:** stop skipping when the literal is dense ([8e5195f](https://github.com/sebastian-software/ferralk/commit/8e5195f4abb7705c10547ad1ffad94dddc065c4a))
* **walker:** evaluate ignore rules per directory ([1f4a9df](https://github.com/sebastian-software/ferralk/commit/1f4a9df37629ad1670658a4a8beeee4317abffb0))
* **walker:** evaluate ignore rules per directory ([df16af5](https://github.com/sebastian-software/ferralk/commit/df16af57175e2ebfe72097785f79e5d95f6c1aac)), closes [#19](https://github.com/sebastian-software/ferralk/issues/19)
* **walker:** keep the prefilters on brace patterns ([198d424](https://github.com/sebastian-software/ferralk/commit/198d4248759931bc99365e9d2061a9887ddfb7a2))
* **walker:** keep the prefilters on brace patterns ([06a4288](https://github.com/sebastian-software/ferralk/commit/06a428887eb78289d5b806de10052dabf89d8f92)), closes [#20](https://github.com/sebastian-software/ferralk/issues/20)
* **walker:** key the follow-symlinks guard on dev and inode ([c365c87](https://github.com/sebastian-software/ferralk/commit/c365c87d47ff9c5a191b90580aa7bff1f5107865))
* **walker:** key the follow-symlinks guard on dev and inode ([197ce36](https://github.com/sebastian-software/ferralk/commit/197ce360d66bd5ebb46574e03d0f81e2828e36e1))
* **walker:** widen the walk below the root ([c9230e2](https://github.com/sebastian-software/ferralk/commit/c9230e203bebacf3a46e6bf151a1fa9050a35abb))
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
