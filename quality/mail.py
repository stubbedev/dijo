import mailbox
import sys
from datetime import datetime, timezone
from email.message import EmailMessage
from email.policy import SMTP
from email.utils import format_datetime, parseaddr
from pathlib import Path

PEOPLE = {
    "Thomas": "Thomas Reid <thomas.reid@example.org>",
    "Margaret": "Margaret Hale <margaret.hale@example.org>",
    "Eleanor": "Eleanor Price <eleanor.price@example.org>",
}


def letters(path):
    for number, block in enumerate(Path(path).read_text(encoding="utf-8").strip().split("\n\n\n")):
        subject, body = block.split("\n\n", 1)
        sender = PEOPLE[body.rstrip().rsplit("\n", 1)[-1]]
        message = EmailMessage(policy=SMTP)
        message["From"] = sender
        message["To"] = ", ".join(person for person in PEOPLE.values() if person != sender)
        message["Subject"] = subject
        sent = datetime(2024, 3, 4 + number, 9, 30, tzinfo=timezone.utc)
        message["Date"] = format_datetime(sent)
        message.set_unixfrom(f"From {parseaddr(sender)[1]} {sent:%a %b %d %H:%M:%S %Y}")
        message["Message-ID"] = f"<letter-{number + 1}@example.org>"
        message.set_content(body + "\n")
        yield message


def main():
    source, eml, mbox = sys.argv[1:]
    first, *rest = letters(source)
    for message in rest:
        first.add_attachment(message)
    first.set_boundary("letters")
    with open(eml, "wb") as out:
        out.write(first.as_bytes())
    partial = Path(mbox + ".partial")
    partial.unlink(missing_ok=True)
    box = mailbox.mbox(partial, create=True)
    try:
        for message in letters(source):
            box.add(message)
    finally:
        box.close()
    partial.replace(mbox)


main()
