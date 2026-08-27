public class UserPhraseData
{
	private int m_a;

	private string b;

	public int OffsetProgrammableMemoryAddress
	{
		set
		{
			this.m_a = value;
		}
	}

	public string UserPhrase
	{
		get { return b; }
	}

	public void b(n7 A_0, int A_1)
	{
		int a_ = 330240 + this.m_a + 32 * A_1;
		A_0.d(UserPhrase, a_, 32);
	}

	public void a(n7 A_0, int A_1)
	{
		int a_ = 330240 + this.m_a + 32 * A_1;
		b = A_0.g(a_, 32);
	}
}
