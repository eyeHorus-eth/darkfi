q = 0x40000000000000000000000000000000224698fc0994a8dd8c46eb2100000001
K = GF(q)
a = K(0x00)
b = K(0x05)
E = EllipticCurve(K, (a, b))
G = E(0x40000000000000000000000000000000224698fc0994a8dd8c46eb2100000000, 0x02)

p = 0x40000000000000000000000000000000224698fc094cf91b992d30ed00000001
assert E.order() == p
Scalar = GF(p)

k = 3
n = 2^k

a = [Scalar(110), Scalar(56),  Scalar(89), Scalar(6543),
     Scalar(2),   Scalar(110), Scalar(44), Scalar(78)]

x = Scalar.random_element()
b = [x^i for i in range(n)]

G = [E.random_element(), E.random_element(), E.random_element(),
     E.random_element(), E.random_element(), E.random_element(),
     E.random_element(), E.random_element()]

assert len(a) == len(b) == len(G) == n

# Dot product
def dot(x, y):
    result = None
    for x_i, y_i in zip(x, y):
        if result is None:
            result = int(x_i) * y_i
        else:
            result += int(x_i) * y_i
    return result

challenges = []
commits = []

original_a, original_G = a, G

# Iterate k times where n = 2^k
for current_k in range(k, 0, -1):
    half = 2^(current_k - 1)
    assert half * 2 == len(a)

    L = dot(a[half:], G[:half])
    R = dot(a[:half], G[half:])
    #z_L = dot(a[half:], b[:half])
    #z_R = dot(a[:half], b[half:])
    commits.append((L, R))

    challenge = Scalar.random_element()
    challenges.append(challenge)

    a = [a[i] + challenge^-1 * a[half + i] for i in range(half)]
    G = [int(challenge^-1) * G[i] + int(challenge) * G[half + i] for i in range(half)]
    assert len(a) == len(G) == half

    # Last iteration
    if current_k == 1:
        assert len(a) == 1
        assert len(G) == 1

        final_a = a[0]
        final_G = G[0]

assert len(challenges) == k

def get_jth_bit(value, idx):
    digits = bin(value)[2:]
    # Add zero padding
    digits = digits.zfill(k)
    return True if digits[idx] == "1" else False

# get scalar values
counters = []
for i in range(1, n + 1):
    s = Scalar(1)
    for j in range(0, k):
        if get_jth_bit(i - 1, j):
            b = 1
        else:
            b = -1
        s *= challenges[j]^b
    counters.append(s)

assert len(counters) == len(original_G)

assert dot(counters, original_G) == final_G

