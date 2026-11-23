//! "How it works" help per page: three short steps, shown in the side panel behind the header's "?".

/// The steps for a sidebar page key, if that page has help.
pub fn steps(page: &str) -> Option<&'static [&'static str]> {
    Some(match page {
        "joingate" => &[
            "When someone joins, Brim checks them against every filter you turned on: account age, no avatar, username, advertising name, suspicious account, suspected spammer and bots.",
            "One matching filter is enough to stop them, unless \"Act only when every enabled filter matches\" is on.",
            "A stopped member gets that filter's trigger action (for example kick or ban). The Join Gate Log tab shows who was stopped and why.",
        ],
        "antinuke" => &[
            "Brim counts the dangerous actions each account does, such as bans, kicks and deleting channels, roles or webhooks, per minute and per hour.",
            "An account that goes past a limit is stopped: Brim quarantines it and undoes what it can (the Undone column in Rule coverage shows what).",
            "When several accounts are stopped in a short time, Panic Mode starts. Members with an exempt permit are not counted.",
        ],
        "automod_panic" | "antinuke_panic" => &[
            "When several accounts are quarantined (Anti Nuke) or punished by Auto Mod in a short time, Brim treats it as an attack.",
            "Panic Mode starts. If \"Lock the server on trigger\" is on, the server is locked for a short time while the attack is contained. A count of 0 turns that trigger off.",
            "Afterwards Brim sends one report of who was contained and what was restored.",
        ],
        "permits" => &[
            "A permit is a set of things a staff member may do with Brim, such as moderating or changing settings.",
            "You give a permit to a person by their Discord user ID, never to a role.",
            "Brim checks the permit, not Discord permissions, so staff need no dangerous Discord rights.",
        ],
        "verification" => &[
            "A new member passes the verification step you choose: a captcha in Discord, in a DM or on the web, or a button.",
            "The time limit pauses while the member is timed out.",
            "If the verification channel is deleted, Brim makes it again and quarantines whoever deleted it. Extra Owners and Trusted Admins get one warning first.",
        ],
        "giveaways" => &[
            "Turn giveaways on, set what a new giveaway starts with, then click Start a giveaway. Brim posts it with an Enter button.",
            "Members press Enter and get a private answer. Brim checks the entry rules at the press and again for each winner at the draw. Quarantined members, Raider List entries and members the Join Gate caught never enter or win.",
            "At the end time Brim draws the winners, posts them and sends each one a DM. Reroll draws all winners again, Redraw replaces one; a past winner never wins the same giveaway again.",
            "Change the look of each message in the Message Builder with the buttons under Messages. Members whose permit has the Manage Giveaways permit perm, and Trusted Admins and up, run giveaways here and with the giveaway command.",
        ],
        "feeds" => &[
            "Paste the address of a website, a YouTube channel, or a GitHub or GitLab repository, and pick the channel to post in. Brim finds a website's feed itself. The first check only remembers what is already there.",
            "A GitHub or GitLab feed gets an address and a secret to paste in the repository's webhook settings. Each event then posts at once, with the look of GitHub's own Discord webhook; pick which events post with the switches.",
            "Every 10 minutes Brim looks for new items and posts at most 3 per feed, through its webhook with the source's name and picture, as if the source posted them. Links to scam or NSFW sites are not posted.",
            "Change the message with Edit message. A feed that fails 5 checks in a row stops and shows a warning on Overview: fix the cause, then click Send latest in its menu.",
        ],
        "lockdown" => &[
            "Lock one channel, a category or the whole server, on a timer if you like.",
            "Brim takes the chat permissions away from your main roles (set them under Settings).",
            "Unlock gives back exactly what was locked, and nothing else.",
        ],
        _ => return None,
    })
}
