# macOS ARM64 runner 更新（2026-11-10）

## TODO

- [x] `build-desktop.yml` の ARM64 runner を `macos-14` から `macos-15` へ変更する。
- [x] 差分が runner 指定の1行だけであり、既存の Rust target・他platform・署名設定を変更しないことを確認する。
- [ ] 変更を反映した workflow で ARM64 job の起動・テスト・DMG生成を確認する。今回の作業では commit／push／再実行は行わない。

## 調査記録

- [v0.99.1 の Build Desktop](https://github.com/kyorohiro/tetorica-fm2612-synth-tauri/actions/runs/42654994919) は、Windows・Linux x86_64／ARM64・macOS Intel の4 job がテスト・ビルド・installer uploadまで成功した。
- [macOS ARM64 job](https://github.com/kyorohiro/tetorica-fm2612-synth-tauri/actions/runs/42654994919/job/129290988243) は runner の割当て前に終了し、step は0件、`runner_id` は0だった。check annotation は `The configuration 'macos-14-arm64-us-default' is not supported`。アプリのコンパイルエラーではない。
- GitHub公式の[macOS 14 retirement告知](https://github.blog/changelog/2026-10-01-github-actions-macos-14-runner-image-retirement/)では、2026-11-02の終了と移行先 `macos-15` が案内されている。
- GitHub公式の[runner一覧](https://docs.github.com/en/actions/how-tos/write-workflows/choose-where-workflows-run/choose-the-runner-for-a-job)で、`macos-15` は ARM64（M1）、`macos-15-intel` は Intel と確認した。ARM64の移行先は `macos-15` とする。

## 検証

- `git diff --check` は成功した。workflow の差分は runner 指定の1行のみ。
- 実際の hosted runner 上での成功は未確認。commit／push／tag変更／Actions再実行／Release操作は行っていない。
