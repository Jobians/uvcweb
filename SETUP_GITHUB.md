# Putting this on GitHub

1. **Push it.**

       git init
       git add -A
       git commit -m "uvcweb: Termux program + Android app"
       git branch -M main
       git remote add origin https://github.com/YOUR_NAME/YOUR_REPO.git
       git push -u origin main

2. **Fix the two badge links** at the top of `README.md`: replace `OWNER/REPO` with your GitHub
   username and repository name (two spots, in the same line).

3. **The Gradle wrapper jar is not included** (it's a binary file and GitHub Actions can fetch it
   for you). Either let CI generate it, or run this once on your own machine and commit the result:

       cd android
       gradle wrapper --gradle-version 8.7   # any local Gradle install works for this one command
       git add gradlew gradlew.bat gradle/wrapper/gradle-wrapper.jar
       git commit -m "add Gradle wrapper"

   The Android workflow calls `./gradlew`, falling back silently if it's missing permission bits,
   but the wrapper **jar itself** has to exist in the repo (or be generated) before that works. If
   you'd rather not deal with this, tell me and I'll switch the workflow to a plain `gradle` action
   that needs no wrapper in the repo at all.

4. **That's it for CI** (`.github/workflows/ci.yml`) and the **Android build**
   (`.github/workflows/android.yml`): both run automatically, no secrets required. The Android
   workflow builds a **debug** APK, which is fine for installing on your own phone.

5. **Optional: signed release APKs.** Without this, tagged releases still contain a debug-signed
   APK. To get one signed with your own key instead:
   - create a keystore: `keytool -genkeypair -v -keystore release.keystore -alias uvcweb -keyalg RSA -keysize 2048 -validity 10000`
   - repo **Settings > Secrets and variables > Actions > Secrets**: add `KEYSTORE_BASE64`
     (`base64 -w0 release.keystore`), `KEYSTORE_PASSWORD`, `KEY_ALIAS`, `KEY_PASSWORD`
   - **Settings > Secrets and variables > Actions > Variables**: add `SIGN_RELEASE` = `true`

6. **Cutting a release:**

       git tag v0.3.0
       git push origin v0.3.0

   `.github/workflows/release.yml` builds everything and attaches it to a new GitHub Release:
   the Android APK, `uvcweb` binaries for Termux (3 CPU types, prebuilt, no NDK needed on the
   phone), and a Linux binary.

## What I could and couldn't check

I don't have GitHub Actions here, so none of these workflows have actually run. I checked, without
running them: every workflow is valid YAML; every `run:` block is syntactically valid bash; the
`android.yml` reusable workflow's inputs/outputs match how `release.yml` calls it; every job's
`needs:` points at a real job; every artifact a job downloads is uploaded by an earlier job; and the
Cargo linker environment variable names the Termux-binary step builds match Cargo's own format.
I could not check things that only show up by actually running on GitHub's runners: exact action
versions, NDK/Gradle version compatibility, and real build/link errors. Send me the failing job's
log and I'll fix it.
