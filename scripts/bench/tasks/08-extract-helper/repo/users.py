def make_username(first, last):
    text = (first + "." + last).strip().lower()
    text = "".join(ch for ch in text if ch.isalnum() or ch == ".")
    return text


def make_handle(nick):
    text = ("@" + nick).strip().lower()
    text = "".join(ch for ch in text if ch.isalnum() or ch == "@")
    return text
