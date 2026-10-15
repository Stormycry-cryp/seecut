# Generation inspiration

The existing generation workbench has Results and Inspiration tabs. Both views stay mounted when switching, preserving browsing state. Only an explicit, valid generation submission selects Results; completion and polling never change the tab.

The initial catalogue contains six original SeeCut **prompt recipes**, two each for product imagery, advertising posters and social/video covers. These are not claimed model outputs or measured quality examples. No stock images, third-party example images or user assets are bundled as generated previews. Verified effect images and a production-provider acceptance run remain a content/release gate.

The scene taxonomy was informed by [YouMind's public prompt collection](https://github.com/YouMind-OpenLab/awesome-gpt-image-2). Its repository declares [CC BY 4.0](https://github.com/YouMind-OpenLab/awesome-gpt-image-2/blob/main/LICENSE), while the README identifies community-sourced examples. That declaration alone does not establish redistribution rights for each third-party image. SeeCut's recipes were written independently and are distributed under this project's AGPL-3.0-or-later license; no repository example image or prompt was copied.

Templates retain the existing `generation-templates/templates.json` store. Optional metadata adds a revision, category, purpose, source, license, operation capability, reference byte versions and a preview reference to a versioned asset in the existing global library. Older manifests still load. Resaving fixes reference versions; unknown older reference versions cannot be silently treated as verified. Personal templates stay personal; saving them grants no public redistribution permission.

“Use this style” validates the current selected model, operation, parameters and every reference before replacing the draft. It never selects a different paid model, submits generation or spends credits. Unspecified curated parameters retain the user's current values. One-step restore retains the prior draft within the same account; an account or model/effort change cannot silently restore incompatible state. Reference uploads use new client identities so late uploads cannot replace restored inputs.

Generated outputs continue through the existing result history, global-library registration and canvas/editor/Flow handoff. Inspiration has no separate generated-results store.
