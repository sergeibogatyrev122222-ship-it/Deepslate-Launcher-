# Setting up Microsoft login

This is the part I can't do for you. It has to be done under your own Microsoft account.

There are two jobs. The first takes about 5 minutes. The second takes about 5 minutes but
then you wait — they review these once a week.

Do both today if you can, because the waiting is the slow part.

---

## Job 1 — Create the app (5 minutes)

This tells Microsoft that a program called Deepslate exists and is allowed to ask people to
log in.

1. Go to **https://portal.azure.com** and sign in with your Microsoft account.
   It's free. You don't need a paid Azure subscription for this.

2. In the search bar at the top, type **App registrations** and click it.

3. Click **New registration**.

4. Fill in the form:

   - **Name:** `Deepslate`

   - **Supported account types:** choose **Personal Microsoft accounts only**.
     This one matters. Minecraft accounts are personal accounts, not work accounts. If you
     pick the wrong option here, logins will fail later and it is annoying to change.

   - **Redirect URI:** change the dropdown from *Web* to
     **Public client/native (mobile & desktop)**, then type `http://localhost` in the box.

     Also important. "Public client" means the app doesn't keep a secret password, which is
     correct for a program that runs on people's own computers — anyone could read a secret
     out of it, so it must not have one.

5. Click **Register**.

6. You'll land on a page with some IDs on it. **Copy these two and send them to me:**

   - **Application (client) ID** — looks like `1a2b3c4d-5e6f-7890-abcd-ef1234567890`
   - **Directory (tenant) ID** — looks the same

   Neither of these is a secret. They're safe to share and they get built into the app.

---

## Job 2 — Ask permission to use Minecraft login (5 minutes, then wait)

Job 1 lets people log in with Microsoft. It does **not** yet let them log in to *Minecraft*.
Mojang keeps a list of approved apps, and you have to ask to be added.

1. Go to **https://aka.ms/mce-reviewappid**

2. Read the rules first: **https://aka.ms/mcusageguidelines**

3. Fill the form in:

   - **Read and understood the EULA:** Yes
   - **Contact information:** your email address. Use the same one as your Azure account —
     they cross-check it.
   - **What type of request:** *New AppID for Approval*
   - **Application Name:** `Deepslate`

     The form bans certain words in app names: *Mojang, Minecraft, Microsoft, Live, Xbox,
     Discord, Hypixel*. "Deepslate" is fine.

   - **Application ID:** the Application (client) ID from Job 1
   - **Tenant ID:** the Directory (tenant) ID from Job 1
   - **Associated website or domain:** they want somewhere they can read about the app. A
     public GitHub repository page works. If you don't have one yet, this is a reason to
     put the project on GitHub.
   - **Justification:** say what it is in a sentence or two. Something like:

     > A desktop Minecraft launcher for personal use. It uses the official Microsoft
     > account sign-in only, checks entitlement through the official endpoints, and does
     > not support unauthenticated accounts or bypass any licence check.

     Submissions with no real justification are not reviewed.

4. Submit it once. **Do not submit it again** — the form says duplicate submissions do not
   speed anything up.

---

## What to expect

**They review submissions weekly.** So the realistic wait is at least a week.

**It might be refused, and I want to be straight with you about that.** Through late 2026
there are developers reporting that their apps still get rejected by Minecraft's servers
with *"Invalid app registration"* even after going through this, and their questions on
Microsoft's own support forum are sitting unanswered. Some guidance points people at the
Xbox developer programme instead, which is really meant for people publishing games, not
launchers. Established launchers like Prism and MultiMC clearly have access, but how they
got it isn't publicly documented.

So: the form is the correct route and it is worth doing, but I can't promise it works.

**This does not block the build.** I'm writing the login code against fake servers that
imitate Microsoft's responses, including all the ways it can fail. That work is real and
tested either way. What we can't do until approval lands is sign in with your actual
account for real.

---

## What happens once you send me the IDs

I wire them in and we can test everything up to the final step. The moment approval comes
through, the last step starts working with no code changes.

Nothing here is secret, so nothing here needs hiding. The client ID is designed to be
public — the security comes from the app having no password at all, not from hiding the ID.
