// The out-of-line helpers of GCC's old C++ library (libstdc++) that games
// built with GCC's headers import: the red-black tree under std::map and
// std::set, and std::__throw_length_error. Game Bridge's own code for the
// same data layout (GCC's _Rb_tree_node_base: { color, parent, left, right },
// color red 0 / black 1). The tree's header node has the root as its parent,
// the leftmost node as left and the rightmost as right; it is red, and an
// empty tree's header points at itself.
//
// The symbols carry GCC's mangled names by hand: libc++'s headers already
// put their own (inline) __throw_length_error into std through std::__1.

#include <stdexcept>

#define EXPORT extern "C" __attribute__((visibility("default")))

namespace {

enum Color { red = 0, black = 1 };

struct Node {
    int color;
    Node* parent;
    Node* left;
    Node* right;
};

Node* leftmost(Node* node) {
    while (node->left)
        node = node->left;
    return node;
}

Node* rightmost(Node* node) {
    while (node->right)
        node = node->right;
    return node;
}

// Turns `node` down to the left: its right child takes its place.
void rotate_left(Node* node, Node*& root) {
    Node* child = node->right;
    node->right = child->left;
    if (child->left)
        child->left->parent = node;
    child->parent = node->parent;
    if (node == root)
        root = child;
    else if (node == node->parent->left)
        node->parent->left = child;
    else
        node->parent->right = child;
    child->left = node;
    node->parent = child;
}

void rotate_right(Node* node, Node*& root) {
    Node* child = node->left;
    node->left = child->right;
    if (child->right)
        child->right->parent = node;
    child->parent = node->parent;
    if (node == root)
        root = child;
    else if (node == node->parent->right)
        node->parent->right = child;
    else
        node->parent->left = child;
    child->right = node;
    node->parent = child;
}

bool is_black(Node* node) { return !node || node->color == black; }

} // namespace

// The next node in order; from the rightmost node, the header.
EXPORT Node* gnu_rb_tree_increment(Node* node) __asm__("_ZSt18_Rb_tree_incrementPSt18_Rb_tree_node_base");
Node* gnu_rb_tree_increment(Node* node) {
    if (node->right)
        return leftmost(node->right);
    Node* parent = node->parent;
    while (node == parent->right) {
        node = parent;
        parent = parent->parent;
    }
    // Climbing from the rightmost node of a tree whose root has no right
    // child ends with `node` at the header and `parent` at the root.
    return node->right == parent ? node : parent;
}

// The previous node in order; from the header (end()), the rightmost node.
EXPORT Node* gnu_rb_tree_decrement(Node* node) __asm__("_ZSt18_Rb_tree_decrementPSt18_Rb_tree_node_base");
Node* gnu_rb_tree_decrement(Node* node) {
    // The header is the red node whose parent's parent is itself (the root's parent).
    if (node->color == red && node->parent->parent == node)
        return node->right;
    if (node->left)
        return rightmost(node->left);
    Node* parent = node->parent;
    while (node == parent->left) {
        node = parent;
        parent = parent->parent;
    }
    return parent;
}

// Links `node` as the left (or right) child of `parent` and restores the
// red-black rules.
EXPORT void gnu_rb_tree_insert_and_rebalance(bool insert_left, Node* node, Node* parent, Node& header)
    __asm__("_ZSt29_Rb_tree_insert_and_rebalancebPSt18_Rb_tree_node_baseS0_RS_");
void gnu_rb_tree_insert_and_rebalance(bool insert_left, Node* node, Node* parent, Node& header) {
    Node*& root = header.parent;
    node->parent = parent;
    node->left = nullptr;
    node->right = nullptr;
    node->color = red;
    if (insert_left) {
        parent->left = node; // also header.left (leftmost) for the first node
        if (parent == &header) {
            root = node;
            header.right = node;
        } else if (parent == header.left) {
            header.left = node;
        }
    } else {
        parent->right = node;
        if (parent == header.right)
            header.right = node;
    }
    // A red node may not have a red parent.
    while (node != root && node->parent->color == red) {
        Node* grandparent = node->parent->parent;
        bool parent_is_left = node->parent == grandparent->left;
        Node* uncle = parent_is_left ? grandparent->right : grandparent->left;
        if (!is_black(uncle)) {
            node->parent->color = black;
            uncle->color = black;
            grandparent->color = red;
            node = grandparent;
            continue;
        }
        if (parent_is_left) {
            if (node == node->parent->right) {
                node = node->parent;
                rotate_left(node, root);
            }
            node->parent->color = black;
            grandparent->color = red;
            rotate_right(grandparent, root);
        } else {
            if (node == node->parent->left) {
                node = node->parent;
                rotate_right(node, root);
            }
            node->parent->color = black;
            grandparent->color = red;
            rotate_left(grandparent, root);
        }
    }
    root->color = black;
}

// Unlinks `node` (the caller frees it), keeps the header's root, leftmost
// and rightmost right and restores the red-black rules; answers `node`.
EXPORT Node* gnu_rb_tree_rebalance_for_erase(Node* node, Node& header)
    __asm__("_ZSt28_Rb_tree_rebalance_for_erasePSt18_Rb_tree_node_baseRS_");
Node* gnu_rb_tree_rebalance_for_erase(Node* node, Node& header) {
    Node*& root = header.parent;
    // `moved`: the node that leaves its place (`node`, or its successor when
    // `node` has two children); `child` takes that place, under `child_parent`.
    Node* moved = node;
    Node* child;
    if (!node->left) {
        child = node->right;
    } else if (!node->right) {
        child = node->left;
    } else {
        moved = leftmost(node->right);
        child = moved->right;
    }
    Node* child_parent;
    auto replace_in_parent = [&](Node* old, Node* replacement) {
        if (root == old)
            root = replacement;
        else if (old->parent->left == old)
            old->parent->left = replacement;
        else
            old->parent->right = replacement;
    };
    int removed_color;
    if (moved != node) {
        // The successor takes `node`'s place, links and color.
        node->left->parent = moved;
        moved->left = node->left;
        if (moved != node->right) {
            child_parent = moved->parent;
            if (child)
                child->parent = moved->parent;
            moved->parent->left = child;
            moved->right = node->right;
            node->right->parent = moved;
        } else {
            child_parent = moved;
        }
        replace_in_parent(node, moved);
        moved->parent = node->parent;
        removed_color = moved->color;
        moved->color = node->color;
    } else {
        child_parent = node->parent;
        if (child)
            child->parent = node->parent;
        replace_in_parent(node, child);
        // `node` had at most one child, so the ends move to a neighbour.
        if (header.left == node)
            header.left = node->right ? leftmost(child) : node->parent;
        if (header.right == node)
            header.right = node->left ? rightmost(child) : node->parent;
        removed_color = node->color;
    }
    // A black node left: `child`'s side is one black short; even it out.
    if (removed_color == black) {
        while (child != root && is_black(child)) {
            if (child == child_parent->left) {
                Node* sibling = child_parent->right;
                if (sibling->color == red) {
                    sibling->color = black;
                    child_parent->color = red;
                    rotate_left(child_parent, root);
                    sibling = child_parent->right;
                }
                if (is_black(sibling->left) && is_black(sibling->right)) {
                    sibling->color = red;
                    child = child_parent;
                    child_parent = child_parent->parent;
                    continue;
                }
                if (is_black(sibling->right)) {
                    sibling->left->color = black;
                    sibling->color = red;
                    rotate_right(sibling, root);
                    sibling = child_parent->right;
                }
                sibling->color = child_parent->color;
                child_parent->color = black;
                if (sibling->right)
                    sibling->right->color = black;
                rotate_left(child_parent, root);
            } else {
                Node* sibling = child_parent->left;
                if (sibling->color == red) {
                    sibling->color = black;
                    child_parent->color = red;
                    rotate_right(child_parent, root);
                    sibling = child_parent->left;
                }
                if (is_black(sibling->right) && is_black(sibling->left)) {
                    sibling->color = red;
                    child = child_parent;
                    child_parent = child_parent->parent;
                    continue;
                }
                if (is_black(sibling->left)) {
                    sibling->right->color = black;
                    sibling->color = red;
                    rotate_left(sibling, root);
                    sibling = child_parent->left;
                }
                sibling->color = child_parent->color;
                child_parent->color = black;
                if (sibling->left)
                    sibling->left->color = black;
                rotate_right(child_parent, root);
            }
            break;
        }
        if (child)
            child->color = black;
    }
    return node;
}

// std::__throw_length_error(const char*): GCC's containers call it when a
// size is too big. libc++'s std::length_error is the same type GCC code
// catches (libc++ keeps its exception classes outside std::__1).
EXPORT __attribute__((noreturn)) void gnu_throw_length_error(const char* what) __asm__("_ZSt20__throw_length_errorPKc");
void gnu_throw_length_error(const char* what) { throw std::length_error(what); }
