import argparse


def main():
    parser = argparse.ArgumentParser(description="Greet someone.")
    parser.add_argument("--name", default="world", help="who to greet")
    parser.add_argument("--times", type=int, default=1, help="how many times")
    parser.add_argument("--loud", action="store_true", help="shout the greeting")
    args = parser.parse_args()
    text = f"Hello, {args.name}!"
    if args.loud:
        text = text.upper()
    for _ in range(args.times):
        print(text)


if __name__ == "__main__":
    main()
