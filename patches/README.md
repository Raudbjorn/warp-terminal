# patches/

Your modifications to **upstream** Warp files live here as patch files — never
edit upstream files in place on `master`. This keeps `master`'s upstream tree
pristine so new Warp releases merge cleanly (`./omw sync`).

- `series` — ordered list of patch files; `omw apply` applies them top-to-bottom.
- `NNNN-*.patch` — `git format-patch` files (carry authorship; re-apply with 3-way merge).

You normally don't hand-edit these. Instead:

```sh
./omw apply                       # land on omw/applied with all patches applied
# ...edit upstream files...
git commit -am "short subject"    # one commit == one patch; subject becomes the filename
./omw save                        # regenerates patches/ + series, back on master
```

New, purely-additive customizations (files Warp doesn't ship) don't need a patch —
just add them to `master` directly; they can't conflict with upstream.
