import time


def application(env, start_response):
    start_response('200', [])

    def body():
        yield b'part1\n'
        time.sleep(1.2)
        yield b'part2\n'
        time.sleep(1.2)
        yield b'part3\n'

    return body()
